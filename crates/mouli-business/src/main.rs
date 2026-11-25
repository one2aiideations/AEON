//! MOULI-BUSINESS: the unified agentic business operating system service.
use axum::{
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    routing::get,
    Json, Router,
};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{env, net::SocketAddr, sync::Arc, time::Duration};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    data: DataAuthority,
    data_configured: bool,
}

#[derive(Clone)]
struct DataAuthority {
    client: Client,
    base_url: Option<String>,
    service_token: Option<String>,
}

#[derive(Debug, Clone)]
struct RequestContext {
    tenant_id: String,
    user_id: String,
    authorization_context: String,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
    detail: String,
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    service: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
struct Readiness {
    status: &'static str,
    mouli_data_configured: bool,
    authorization_boundary: &'static str,
    note: &'static str,
}

#[derive(Debug, Deserialize)]
struct MoneyInput {
    amount: Decimal,
    currency: String,
}

impl MoneyInput {
    fn validate(&self) -> Result<(), String> {
        if self.amount < Decimal::ZERO {
            return Err("amount cannot be negative".into());
        }
        if self.currency.trim().len() != 3
            || !self.currency.chars().all(|c| c.is_ascii_alphabetic())
        {
            return Err("currency must be a three-letter ISO-style code".into());
        }
        Ok(())
    }
}

impl DataAuthority {
    fn new() -> Self {
        let base_url = env::var("MOULI_DATA_API_BASE")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let service_token = env::var("MOULI_DATA_SERVICE_TOKEN").ok();
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client configuration must be valid"),
            base_url,
            service_token,
        }
    }

    async fn request(
        &self,
        method: Method,
        resource: &str,
        id: Option<&str>,
        context: &RequestContext,
        body: Option<Value>,
    ) -> Result<Value, (StatusCode, String)> {
        let base = self.base_url.as_ref().ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "MOULI-DATA adapter is not configured; set MOULI_DATA_API_BASE after verifying the real MOULI-DATA contract".into(),
        ))?;

        let safe_resource = resource
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !safe_resource {
            return Err((StatusCode::BAD_REQUEST, "invalid resource name".into()));
        }

        let mut url = format!("{}/v1/business/{}", base.trim_end_matches('/'), resource);
        if let Some(record_id) = id {
            if record_id.contains('/') || record_id.contains('?') || record_id.contains('#') {
                return Err((StatusCode::BAD_REQUEST, "invalid record identifier".into()));
            }
            url.push('/');
            url.push_str(record_id);
        }

        let mut request = self
            .client
            .request(method, url)
            .header("x-mouli-tenant-id", &context.tenant_id)
            .header("x-mouli-user-id", &context.user_id)
            .header(
                "x-mouli-authorization-context",
                &context.authorization_context,
            );

        if let Some(token) = &self.service_token {
            request = request.bearer_auth(token);
        }
        if let Some(payload) = body {
            request = request.json(&payload);
        }

        let response = request.send().await.map_err(|e| {
            error!(error = %e, "MOULI-DATA request failed");
            (StatusCode::BAD_GATEWAY, "MOULI-DATA request failed".into())
        })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|_| {
            (
                StatusCode::BAD_GATEWAY,
                "could not read MOULI-DATA response".into(),
            )
        })?;
        if !status.is_success() {
            let detail = String::from_utf8_lossy(&bytes)
                .chars()
                .take(500)
                .collect::<String>();
            return Err((
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                detail,
            ));
        }
        if bytes.is_empty() {
            return Ok(json!({"ok": true}));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            (
                StatusCode::BAD_GATEWAY,
                "MOULI-DATA returned a non-JSON response".into(),
            )
        })
    }
}

// This boundary expects a trusted MOULI-AUTH gateway to authenticate the caller,
// authorize the request, and inject these headers. Never trust client-supplied
// copies of these headers on a publicly exposed listener.
fn request_context(headers: &HeaderMap) -> Result<RequestContext, (StatusCode, String)> {
    let get = |name: &'static str| -> Result<String, (StatusCode, String)> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.trim().is_empty())
            .map(str::to_owned)
            .ok_or((
                StatusCode::UNAUTHORIZED,
                format!("missing trusted gateway context: {name}"),
            ))
    };
    Ok(RequestContext {
        tenant_id: get("x-mouli-tenant-id")?,
        user_id: get("x-mouli-user-id")?,
        authorization_context: get("x-mouli-authorization-context")?,
    })
}

fn validate_record(resource: &str, value: &Value) -> Result<(), String> {
    let required = |key: &str| -> Result<(), String> {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(|_| ())
            .ok_or_else(|| format!("'{key}' is required and must be a non-empty string"))
    };
    match resource {
        "customers" | "companies" | "suppliers" | "contacts" | "products" | "employees" => {
            required("name")
        }
        "leads" => {
            required("name")?;
            required("source")
        }
        "opportunities" => {
            required("name")?;
            required("stage")?;
            let amount = value
                .get("amount")
                .and_then(Value::as_str)
                .ok_or("amount must be a decimal string")?
                .parse::<Decimal>()
                .map_err(|_| "amount is not a valid decimal")?;
            let currency = value
                .get("currency")
                .and_then(Value::as_str)
                .ok_or("currency is required")?
                .to_owned();
            MoneyInput { amount, currency }.validate()
        }
        "sales-orders" | "purchase-orders" => {
            required("customer_or_supplier_id")?;
            required("currency")?;
            let amount = value
                .get("total")
                .and_then(Value::as_str)
                .ok_or("total must be a decimal string")?;
            let amount = amount
                .parse::<Decimal>()
                .map_err(|_| "total is not a valid decimal")?;
            if amount < Decimal::ZERO {
                return Err("total cannot be negative".into());
            }
            Ok(())
        }
        "journal-entries" => {
            let debit = value
                .get("debit_total")
                .and_then(Value::as_str)
                .ok_or("debit_total must be a decimal string")?
                .parse::<Decimal>()
                .map_err(|_| "debit_total is invalid")?;
            let credit = value
                .get("credit_total")
                .and_then(Value::as_str)
                .ok_or("credit_total must be a decimal string")?
                .parse::<Decimal>()
                .map_err(|_| "credit_total is invalid")?;
            if debit < Decimal::ZERO || credit < Decimal::ZERO {
                return Err("journal totals cannot be negative".into());
            }
            if debit != credit {
                return Err(
                    "journal entry is not balanced: debit_total must equal credit_total".into(),
                );
            }
            Ok(())
        }
        "inventory-items" => {
            required("sku")?;
            let quantity = value
                .get("quantity")
                .and_then(Value::as_i64)
                .ok_or("quantity must be an integer")?;
            if quantity < 0 {
                return Err("quantity cannot be negative".into());
            }
            Ok(())
        }
        "tickets" => {
            required("subject")?;
            required("status")
        }
        "campaigns" => {
            required("name")?;
            required("channel")
        }
        "projects" => {
            required("name")?;
            required("status")
        }
        _ => Err(format!("unsupported business resource: {resource}")),
    }
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        service: "mouli-business",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn readiness(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Readiness>) {
    let ready = state.data_configured;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(Readiness {
        status: if ready { "ready_for_adapter_calls" } else { "not_ready" },
        mouli_data_configured: ready,
        authorization_boundary: "trusted_mouli_auth_gateway_required",
        note: "MOULI-DATA endpoint compatibility and MOULI-AUTH authorization enforcement must be verified before production",
    }))
}

async fn list_records(
    State(state): State<Arc<AppState>>,
    Path(resource): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let context = request_context(&headers).map_err(api_error)?;
    let result = state
        .data
        .request(Method::GET, &resource, None, &context, None)
        .await
        .map_err(api_error)?;
    Ok(Json(result))
}

async fn get_record(
    State(state): State<Arc<AppState>>,
    Path((resource, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let context = request_context(&headers).map_err(api_error)?;
    let result = state
        .data
        .request(Method::GET, &resource, Some(&id), &context, None)
        .await
        .map_err(api_error)?;
    Ok(Json(result))
}

async fn create_record(
    State(state): State<Arc<AppState>>,
    Path(resource): Path<String>,
    headers: HeaderMap,
    Json(mut payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<ApiError>)> {
    let context = request_context(&headers).map_err(api_error)?;
    validate_record(&resource, &payload)
        .map_err(|e| api_error((StatusCode::UNPROCESSABLE_ENTITY, e)))?;
    if !payload.is_object() {
        return Err(api_error((
            StatusCode::BAD_REQUEST,
            "record must be a JSON object".into(),
        )));
    }
    if payload.get("id").is_none() {
        payload["id"] = json!(Uuid::new_v4().to_string());
    }
    payload["tenant_id"] = json!(context.tenant_id.clone());
    payload["created_by"] = json!(context.user_id.clone());
    let result = state
        .data
        .request(Method::POST, &resource, None, &context, Some(payload))
        .await
        .map_err(api_error)?;
    Ok((StatusCode::CREATED, Json(result)))
}

async fn update_record(
    State(state): State<Arc<AppState>>,
    Path((resource, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let context = request_context(&headers).map_err(api_error)?;
    validate_record(&resource, &payload)
        .map_err(|e| api_error((StatusCode::UNPROCESSABLE_ENTITY, e)))?;
    let result = state
        .data
        .request(Method::PUT, &resource, Some(&id), &context, Some(payload))
        .await
        .map_err(api_error)?;
    Ok(Json(result))
}

fn api_error(error: (StatusCode, String)) -> (StatusCode, Json<ApiError>) {
    (
        error.0,
        Json(ApiError {
            error: error.0.to_string(),
            detail: error.1,
        }),
    )
}

fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .route("/api/v1/:resource", get(list_records).post(create_record))
        .route("/api/v1/:resource/:id", get(get_record).put(update_record))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let data = DataAuthority::new();
    let data_configured = data.base_url.is_some();
    let state = Arc::new(AppState {
        data,
        data_configured,
    });

    let host = env::var("MOULI_BUSINESS_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = env::var("MOULI_BUSINESS_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(8088);
    let address: SocketAddr = format!("{host}:{port}")
        .parse()
        .expect("MOULI_BUSINESS_HOST and MOULI_BUSINESS_PORT must form a valid socket address");

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("could not bind MOULI-BUSINESS listener");
    info!(%address, data_configured, "MOULI-BUSINESS server starting");
    if !data_configured {
        info!("MOULI_DATA_API_BASE is unset; business data routes will return 503");
    }
    axum::serve(listener, app(state))
        .await
        .expect("server failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_must_balance() {
        let valid = json!({"debit_total":"1250.00","credit_total":"1250.00"});
        let invalid = json!({"debit_total":"1250.00","credit_total":"1249.99"});
        assert!(validate_record("journal-entries", &valid).is_ok());
        assert!(validate_record("journal-entries", &invalid).is_err());
    }

    #[test]
    fn opportunity_requires_name_stage_and_nonnegative_decimal_amount() {
        assert!(validate_record("opportunities", &json!({
            "name":"Factory automation","stage":"qualification","amount":"25000.00","currency":"INR"
        })).is_ok());
        assert!(validate_record(
            "opportunities",
            &json!({
                "name":"Invalid","stage":"qualification","amount":"-1.00","currency":"INR"
            })
        )
        .is_err());
    }

    #[test]
    fn inventory_quantity_cannot_be_negative() {
        assert!(validate_record("inventory-items", &json!({"sku":"SKU-1","quantity":10})).is_ok());
        assert!(validate_record("inventory-items", &json!({"sku":"SKU-1","quantity":-1})).is_err());
    }

    #[test]
    fn lead_requires_source() {
        assert!(validate_record("leads", &json!({"name":"Prospect","source":"website"})).is_ok());
        assert!(validate_record("leads", &json!({"name":"Prospect"})).is_err());
    }

    #[test]
    fn money_validation_uses_decimal() {
        let money = MoneyInput {
            amount: "0.10".parse().unwrap(),
            currency: "INR".into(),
        };
        assert!(money.validate().is_ok());
    }
}
