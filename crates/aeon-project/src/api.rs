//! Local REST API. Binds to localhost only (no authentication yet).
//! Every successful change is written to the database before replying.

use crate::db::Db;
use crate::model::{Project, Store, Task};
use crate::schedule::{critical_path, ScheduleResult};
use crate::{ProjectError, APP_NAME};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

/// Shared application state.
#[derive(Clone, Debug)]
pub struct AppState {
    /// Data.
    pub store: Arc<Mutex<Store>>,
    /// Persistence.
    pub db: Arc<Mutex<Db>>,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

fn status_of(e: &ProjectError) -> StatusCode {
    match e {
        ProjectError::NotFound(_) => StatusCode::NOT_FOUND,
        ProjectError::Invalid(_) | ProjectError::Cycle => StatusCode::UNPROCESSABLE_ENTITY,
        ProjectError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        ProjectError::Ai(_) => StatusCode::BAD_GATEWAY,
    }
}

fn fail(e: ProjectError) -> (StatusCode, String) {
    (status_of(&e), e.to_string())
}

fn poisoned() -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "state lock poisoned".into(),
    )
}

/// Apply a change and persist it; on a storage error the change is rolled back.
fn mutate<T>(
    st: &AppState,
    f: impl FnOnce(&mut Store) -> Result<T, ProjectError>,
) -> Result<T, (StatusCode, String)> {
    let mut store = st.store.lock().map_err(|_| poisoned())?;
    let before = store.clone();
    let out = f(&mut store).map_err(fail)?;
    let db = st.db.lock().map_err(|_| poisoned())?;
    if let Err(e) = db.save(&store) {
        *store = before;
        return Err(fail(e));
    }
    Ok(out)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "application": APP_NAME,
        "status": "running",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn list_projects(State(st): State<AppState>) -> ApiResult<Vec<Project>> {
    Ok(Json(
        st.store.lock().map_err(|_| poisoned())?.projects.clone(),
    ))
}

async fn list_tasks(State(st): State<AppState>) -> ApiResult<Vec<Task>> {
    Ok(Json(st.store.lock().map_err(|_| poisoned())?.tasks.clone()))
}

#[derive(Deserialize)]
struct NewProject {
    name: String,
    #[serde(default)]
    description: String,
}

async fn create_project(
    State(st): State<AppState>,
    Json(input): Json<NewProject>,
) -> ApiResult<Project> {
    let p = mutate(&st, |s| {
        let id = s.add_project(&input.name)?;
        let p = s
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ProjectError::NotFound(id.clone()))?;
        p.description = input.description;
        Ok(p.clone())
    })?;
    Ok(Json(p))
}

#[derive(Deserialize)]
struct NewTask {
    project_id: String,
    title: String,
    #[serde(default)]
    estimated_hours: f64,
    #[serde(default)]
    assignee: String,
}

async fn create_task(State(st): State<AppState>, Json(input): Json<NewTask>) -> ApiResult<Task> {
    if !input.estimated_hours.is_finite() || input.estimated_hours < 0.0 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "estimated_hours must be >= 0".into(),
        ));
    }
    let t = mutate(&st, |s| {
        let id = s.add_task(&input.project_id, &input.title)?;
        let t = s
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| ProjectError::NotFound(id.clone()))?;
        t.estimated_hours = input.estimated_hours;
        t.assignee = input.assignee;
        Ok(t.clone())
    })?;
    Ok(Json(t))
}

#[derive(Deserialize)]
struct StatusChange {
    status: String,
}

async fn set_status(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<StatusChange>,
) -> ApiResult<serde_json::Value> {
    mutate(&st, |s| s.update_task_status(&id, &input.status))?;
    Ok(Json(serde_json::json!({"id": id, "status": input.status})))
}

#[derive(Deserialize)]
struct NewDependency {
    depends_on: String,
}

async fn add_dependency(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NewDependency>,
) -> ApiResult<serde_json::Value> {
    mutate(&st, |s| s.add_dependency(&id, &input.depends_on))?;
    Ok(Json(
        serde_json::json!({"id": id, "depends_on": input.depends_on}),
    ))
}

async fn schedule(
    State(st): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<Vec<ScheduleResult>> {
    let tasks = st
        .store
        .lock()
        .map_err(|_| poisoned())?
        .project_tasks(&project_id);
    critical_path(&tasks).map(Json).map_err(fail)
}

/// Build the router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/:id/schedule", get(schedule))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/:id/status", patch(set_status))
        .route("/tasks/:id/dependencies", post(add_dependency))
        .with_state(state)
}

/// Serve on `addr` (use a localhost address).
pub async fn serve(state: AppState, addr: &str) -> Result<(), ProjectError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| ProjectError::Storage(format!("bind {addr}: {e}")))?;
    axum::serve(listener, router(state))
        .await
        .map_err(|e| ProjectError::Storage(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn state() -> AppState {
        AppState {
            store: Arc::new(Mutex::new(Store::default())),
            db: Arc::new(Mutex::new(Db::in_memory().unwrap())),
        }
    }

    async fn call(
        st: &AppState,
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = router(st.clone()).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::String(
            String::from_utf8_lossy(&bytes).into(),
        ));
        (status, json)
    }

    #[tokio::test]
    async fn create_flow_persists_and_validates() {
        let st = state();
        let (s, p) = call(
            &st,
            "POST",
            "/projects",
            serde_json::json!({"name": "Launch"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let pid = p["id"].as_str().unwrap().to_string();
        let (s, _) = call(&st, "POST", "/projects", serde_json::json!({"name": " "})).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

        let (s, a) = call(
            &st,
            "POST",
            "/tasks",
            serde_json::json!({"project_id": pid, "title": "Design", "estimated_hours": 4}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (_, b) = call(
            &st,
            "POST",
            "/tasks",
            serde_json::json!({"project_id": pid, "title": "Build", "estimated_hours": 6}),
        )
        .await;
        let (a, b) = (
            a["id"].as_str().unwrap().to_string(),
            b["id"].as_str().unwrap().to_string(),
        );
        let (s, _) = call(
            &st,
            "POST",
            "/tasks",
            serde_json::json!({"project_id": "nope", "title": "X"}),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        let (s, _) = call(
            &st,
            "POST",
            &format!("/tasks/{b}/dependencies"),
            serde_json::json!({"depends_on": a}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(
            &st,
            "POST",
            &format!("/tasks/{a}/dependencies"),
            serde_json::json!({"depends_on": b}),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "cycle rejected");

        let (s, sched) = call(
            &st,
            "GET",
            &format!("/projects/{pid}/schedule"),
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(sched.as_array().unwrap().len(), 2);

        let (s, _) = call(
            &st,
            "PATCH",
            &format!("/tasks/{a}/status"),
            serde_json::json!({"status": "Done"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(
            &st,
            "PATCH",
            &format!("/tasks/{a}/status"),
            serde_json::json!({"status": "Nope"}),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

        let saved = st.db.lock().unwrap().load().unwrap();
        assert_eq!(saved.tasks.len(), 2, "changes were persisted");
        assert_eq!(
            saved.tasks.iter().find(|t| t.id == a).unwrap().status,
            "Done"
        );
        let (_, h) = call(&st, "GET", "/health", serde_json::Value::Null).await;
        assert_eq!(h["status"], "running");
    }
}
