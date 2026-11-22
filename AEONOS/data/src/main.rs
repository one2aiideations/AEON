//! AEON-DATA — single-file local-first data authority.
//!
//! The canonical JSON record store is the source of truth. Graph and vector
//! tables are first-class indexed representations within the same redb file.
//! This implementation is an intentionally honest foundation: vector search
//! is exact scan; advanced SQL, distributed consensus, cryptographic key
//! management, and evolutionary AEON-RAG are not claimed as implemented.

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};
use tracing::{info, warn};
use uuid::Uuid;

const RECORDS: TableDefinition<&str, &str> = TableDefinition::new("records");
const CATALOG: TableDefinition<&str, &str> = TableDefinition::new("catalog");
const GRAPH_NODES: TableDefinition<&str, &str> = TableDefinition::new("graph_nodes");
const GRAPH_EDGES: TableDefinition<&str, &str> = TableDefinition::new("graph_edges");
const VECTORS: TableDefinition<&str, &str> = TableDefinition::new("vectors");
const AUDIT: TableDefinition<&str, &str> = TableDefinition::new("audit");

#[derive(Clone)]
struct Engine {
    db: Arc<Database>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    namespace: String,
    value: Value,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphNode {
    id: String,
    namespace: String,
    labels: Vec<String>,
    properties: Value,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphEdge {
    id: String,
    namespace: String,
    from: String,
    to: String,
    kind: String,
    properties: Value,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VectorEntry {
    id: String,
    namespace: String,
    values: Vec<f32>,
    metadata: Value,
    model: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuditEvent {
    id: String,
    namespace: String,
    actor: String,
    action: String,
    resource: String,
    at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct PutRecord {
    value: Value,
}

#[derive(Debug, Deserialize)]
struct NewNode {
    id: Option<String>,
    labels: Vec<String>,
    #[serde(default = "empty_object")]
    properties: Value,
}

#[derive(Debug, Deserialize)]
struct NewEdge {
    id: Option<String>,
    from: String,
    to: String,
    kind: String,
    #[serde(default = "empty_object")]
    properties: Value,
}

#[derive(Debug, Deserialize)]
struct NewVector {
    id: Option<String>,
    values: Vec<f32>,
    #[serde(default = "empty_object")]
    metadata: Value,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VectorSearch {
    query: Vec<f32>,
    #[serde(default = "default_metric")]
    metric: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

#[derive(Debug, Serialize)]
struct VectorHit {
    entry: VectorEntry,
    score: f32,
}

#[derive(Clone)]
struct ApiState {
    engine: Engine,
    token: String,
    namespace: String,
    role: String,
}

#[derive(Parser)]
#[command(name = "aeon-data", version, about = "AEON unified local data authority")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Put {
        #[arg(long, default_value = "local")]
        namespace: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        json: String,
    },
    Get {
        #[arg(long, default_value = "local")]
        namespace: String,
        #[arg(long)]
        key: String,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        bind: SocketAddr,
    },
    Stats,
}

fn empty_object() -> Value {
    json!({})
}

fn default_metric() -> String {
    "cosine".to_owned()
}

fn default_top_k() -> usize {
    5
}

fn scoped_key(namespace: &str, key: &str) -> Result<String> {
    validate_namespace(namespace)?;
    if key.trim().is_empty() || key.contains('\0') {
        return Err(anyhow!("key must be non-empty and contain no NUL"));
    }
    Ok(format!("{namespace}\0{key}"))
}

fn validate_namespace(namespace: &str) -> Result<()> {
    if namespace.is_empty()
        || namespace.len() > 160
        || !namespace
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(anyhow!("invalid namespace"));
    }
    Ok(())
}

impl Engine {
    fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating database directory {}", parent.display()))?;
            }
        }
        let db = Database::create(&path)
            .with_context(|| format!("opening AEON-DATA database {}", path.display()))?;
        {
            let tx = db.begin_write()?;
            tx.open_table(RECORDS)?;
            tx.open_table(CATALOG)?;
            tx.open_table(GRAPH_NODES)?;
            tx.open_table(GRAPH_EDGES)?;
            tx.open_table(VECTORS)?;
            tx.open_table(AUDIT)?;
            tx.commit()?;
        }
        Ok(Self { db: Arc::new(db) })
    }

    fn put_record(&self, namespace: &str, key: &str, value: Value, actor: &str) -> Result<Record> {
        let storage_key = scoped_key(namespace, key)?;
        let now = Utc::now();
        let tx = self.db.begin_write()?;
        let mut table = tx.open_table(RECORDS)?;
        let previous = table.get(storage_key.as_str())?;
        let (version, created_at) = if let Some(old) = previous {
            let old: Record = serde_json::from_str(old.value())?;
            (old.version + 1, old.created_at)
        } else {
            (1, now)
        };
        drop(previous);
        let record = Record {
            id: key.to_owned(),
            namespace: namespace.to_owned(),
            value,
            version,
            created_at,
            updated_at: now,
        };
        let encoded = serde_json::to_string(&record)?;
        table.insert(storage_key.as_str(), encoded.as_str())?;
        drop(table);
        append_audit_tx(&tx, namespace, actor, "record.put", key)?;
        tx.commit()?;
        Ok(record)
    }

    fn get_record(&self, namespace: &str, key: &str) -> Result<Option<Record>> {
        let storage_key = scoped_key(namespace, key)?;
        let tx = self.db.begin_read()?;
        let table = tx.open_table(RECORDS)?;
        let found = table
            .get(storage_key.as_str())?
            .map(|v| serde_json::from_str(v.value()))
            .transpose()?;
        Ok(found)
    }

    fn put_node(&self, namespace: &str, input: NewNode, actor: &str) -> Result<GraphNode> {
        validate_namespace(namespace)?;
        let id = input.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let key = scoped_key(namespace, &id)?;
        let node = GraphNode {
            id: id.clone(),
            namespace: namespace.to_owned(),
            labels: input.labels,
            properties: input.properties,
            created_at: Utc::now(),
        };
        let encoded = serde_json::to_string(&node)?;
        let tx = self.db.begin_write()?;
        tx.open_table(GRAPH_NODES)?.insert(key.as_str(), encoded.as_str())?;
        append_audit_tx(&tx, namespace, actor, "graph.node.create", &id)?;
        tx.commit()?;
        Ok(node)
    }

    fn put_edge(&self, namespace: &str, input: NewEdge, actor: &str) -> Result<GraphEdge> {
        validate_namespace(namespace)?;
        let from_key = scoped_key(namespace, &input.from)?;
        let to_key = scoped_key(namespace, &input.to)?;
        {
            let tx = self.db.begin_read()?;
            let nodes = tx.open_table(GRAPH_NODES)?;
            if nodes.get(from_key.as_str())?.is_none() || nodes.get(to_key.as_str())?.is_none() {
                return Err(anyhow!("both endpoint nodes must exist in this namespace"));
            }
        }
        let id = input.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let key = scoped_key(namespace, &id)?;
        let edge = GraphEdge {
            id: id.clone(),
            namespace: namespace.to_owned(),
            from: input.from,
            to: input.to,
            kind: input.kind,
            properties: input.properties,
            created_at: Utc::now(),
        };
        let encoded = serde_json::to_string(&edge)?;
        let tx = self.db.begin_write()?;
        tx.open_table(GRAPH_EDGES)?.insert(key.as_str(), encoded.as_str())?;
        append_audit_tx(&tx, namespace, actor, "graph.edge.create", &id)?;
        tx.commit()?;
        Ok(edge)
    }

    fn put_vector(&self, namespace: &str, input: NewVector, actor: &str) -> Result<VectorEntry> {
        validate_namespace(namespace)?;
        if input.values.is_empty() || input.values.len() > 65_536 {
            return Err(anyhow!("vector dimension must be between 1 and 65536"));
        }
        if input.values.iter().any(|v| !v.is_finite()) {
            return Err(anyhow!("vector values must be finite"));
        }
        let id = input.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let key = scoped_key(namespace, &id)?;
        let entry = VectorEntry {
            id: id.clone(),
            namespace: namespace.to_owned(),
            values: input.values,
            metadata: input.metadata,
            model: input.model,
            created_at: Utc::now(),
        };
        let encoded = serde_json::to_string(&entry)?;
        let tx = self.db.begin_write()?;
        tx.open_table(VECTORS)?.insert(key.as_str(), encoded.as_str())?;
        append_audit_tx(&tx, namespace, actor, "vector.insert", &id)?;
        tx.commit()?;
        Ok(entry)
    }

    fn search_vectors(
        &self,
        namespace: &str,
        query: &[f32],
        metric: &str,
        top_k: usize,
    ) -> Result<Vec<VectorHit>> {
        validate_namespace(namespace)?;
        if query.is_empty() || query.iter().any(|v| !v.is_finite()) {
            return Err(anyhow!("query vector must contain finite values"));
        }
        if !matches!(metric, "cosine" | "dot" | "euclidean") {
            return Err(anyhow!("metric must be cosine, dot, or euclidean"));
        }
        let prefix = format!("{namespace}\0");
        let tx = self.db.begin_read()?;
        let table = tx.open_table(VECTORS)?;
        let mut hits = Vec::new();
        for item in table.iter()? {
            let (key, value) = item?;
            if !key.value().starts_with(&prefix) {
                continue;
            }
            let entry: VectorEntry = serde_json::from_str(value.value())?;
            if entry.values.len() != query.len() {
                continue;
            }
            let score = match metric {
                "cosine" => cosine_similarity(query, &entry.values),
                "dot" => query.iter().zip(&entry.values).map(|(a, b)| a * b).sum(),
                "euclidean" => -query
                    .iter()
                    .zip(&entry.values)
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f32>()
                    .sqrt(),
                _ => unreachable!(),
            };
            hits.push(VectorHit { entry, score });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(top_k.clamp(1, 1000));
        Ok(hits)
    }

    fn stats(&self) -> Result<Value> {
        let tx = self.db.begin_read()?;
        let records = tx.open_table(RECORDS)?.len()?;
        let nodes = tx.open_table(GRAPH_NODES)?.len()?;
        let edges = tx.open_table(GRAPH_EDGES)?.len()?;
        let vectors = tx.open_table(VECTORS)?.len()?;
        let catalog = tx.open_table(CATALOG)?.len()?;
        Ok(json!({
            "engine": "redb",
            "records": records,
            "graph_nodes": nodes,
            "graph_edges": edges,
            "vectors": vectors,
            "catalog_entries": catalog,
            "vector_index": "exact_scan",
            "status": "operational"
        }))
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

fn append_audit_tx(
    tx: &redb::WriteTransaction,
    namespace: &str,
    actor: &str,
    action: &str,
    resource: &str,
) -> Result<()> {
    let event = AuditEvent {
        id: Uuid::new_v4().to_string(),
        namespace: namespace.to_owned(),
        actor: actor.to_owned(),
        action: action.to_owned(),
        resource: resource.to_owned(),
        at: Utc::now(),
    };
    let key = format!("{}\0{}", namespace, event.id);
    let value = serde_json::to_string(&event)?;
    tx.open_table(AUDIT)?.insert(key.as_str(), value.as_str())?;
    Ok(())
}

fn authorize(headers: &HeaderMap, state: &ApiState, write: bool) -> Result<(), StatusCode> {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if supplied != state.token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if write && !matches!(state.role.as_str(), "writer" | "admin") {
        return Err(StatusCode::FORBIDDEN);
    }
    if !write && !matches!(state.role.as_str(), "reader" | "writer" | "admin") {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ok","service":"aeon-data","mode":"local-first"}))
}

async fn api_get_record(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path((namespace, key)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state, false)?;
    if namespace != state.namespace {
        return Err(StatusCode::FORBIDDEN);
    }
    let record = state
        .engine
        .get_record(&namespace, &key)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::to_value(record).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?))
}

async fn api_put_record(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path((namespace, key)): Path<(String, String)>,
    Json(body): Json<PutRecord>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state, true)?;
    if namespace != state.namespace {
        return Err(StatusCode::FORBIDDEN);
    }
    let record = state
        .engine
        .put_record(&namespace, &key, body.value, "http-bearer")
        .map_err(|e| {
            warn!(error = %e, "record write failed");
            StatusCode::BAD_REQUEST
        })?;
    Ok(Json(serde_json::to_value(record).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?))
}

async fn api_put_node(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
    Json(body): Json<NewNode>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state, true)?;
    if namespace != state.namespace {
        return Err(StatusCode::FORBIDDEN);
    }
    let node = state.engine.put_node(&namespace, body, "http-bearer")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::to_value(node).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?))
}

async fn api_put_edge(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
    Json(body): Json<NewEdge>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state, true)?;
    if namespace != state.namespace {
        return Err(StatusCode::FORBIDDEN);
    }
    let edge = state.engine.put_edge(&namespace, body, "http-bearer")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::to_value(edge).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?))
}

async fn api_put_vector(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
    Json(body): Json<NewVector>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state, true)?;
    if namespace != state.namespace {
        return Err(StatusCode::FORBIDDEN);
    }
    let vector = state.engine.put_vector(&namespace, body, "http-bearer")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::to_value(vector).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?))
}

async fn api_search_vectors(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
    Json(body): Json<VectorSearch>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state, false)?;
    if namespace != state.namespace {
        return Err(StatusCode::FORBIDDEN);
    }
    let hits = state.engine.search_vectors(&namespace, &body.query, &body.metric, body.top_k)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(json!({"hits": hits, "strategy":"exact_scan"})))
}

fn database_path() -> PathBuf {
    env::var_os("AEON_DATA_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./aeon-data.redb"))
}

async fn serve(engine: Engine, bind: SocketAddr) -> Result<()> {
    let token = env::var("AEON_DATA_TOKEN")
        .context("AEON_DATA_TOKEN is required; refusing to start an unauthenticated server")?;
    if token.len() < 24 {
        return Err(anyhow!("AEON_DATA_TOKEN must contain at least 24 characters"));
    }
    let namespace = env::var("AEON_DATA_NAMESPACE").unwrap_or_else(|_| "local".to_owned());
    validate_namespace(&namespace)?;
    let role = env::var("AEON_DATA_ROLE").unwrap_or_else(|_| "admin".to_owned());
    if !matches!(role.as_str(), "reader" | "writer" | "admin") {
        return Err(anyhow!("AEON_DATA_ROLE must be reader, writer, or admin"));
    }
    let state = ApiState { engine, token, namespace, role };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/records/:namespace/:key", get(api_get_record).put(api_put_record))
        .route("/v1/graph/:namespace/nodes", post(api_put_node))
        .route("/v1/graph/:namespace/edges", post(api_put_edge))
        .route("/v1/vectors/:namespace", post(api_put_vector))
        .route("/v1/vectors/:namespace/search", post(api_search_vectors))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(address = %bind, "AEON-DATA API listening");
    axum::serve(listener, app).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()))
        .init();

    let cli = Cli::parse();
    let engine = Engine::open(database_path())?;
    match cli.command {
        Command::Init => {
            println!("{}", serde_json::to_string_pretty(&engine.stats()?)?);
        }
        Command::Put { namespace, key, json: raw } => {
            let value: Value = serde_json::from_str(&raw).context("invalid JSON payload")?;
            let record = engine.put_record(&namespace, &key, value, "local-cli")?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Get { namespace, key } => {
            match engine.get_record(&namespace, &key)? {
                Some(record) => println!("{}", serde_json::to_string_pretty(&record)?),
                None => println!("null"),
            }
        }
        Command::Serve { bind } => serve(engine, bind).await?,
        Command::Stats => println!("{}", serde_json::to_string_pretty(&engine.stats()?)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn records_persist_and_are_namespace_scoped() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("test.redb");
        let engine = Engine::open(&path)?;
        engine.put_record("tenant-a", "customer-1", json!({"name":"Ada"}), "test")?;
        assert!(engine.get_record("tenant-a", "customer-1")?.is_some());
        assert!(engine.get_record("tenant-b", "customer-1")?.is_none());
        drop(engine);
        let reopened = Engine::open(&path)?;
        assert_eq!(
            reopened.get_record("tenant-a", "customer-1")?.unwrap().value["name"],
            "Ada"
        );
        Ok(())
    }

    #[test]
    fn cosine_search_ranks_matching_vector_first() -> Result<()> {
        let dir = tempdir()?;
        let engine = Engine::open(dir.path().join("vectors.redb"))?;
        engine.put_vector("local", NewVector {
            id: Some("same".into()),
            values: vec![1.0, 0.0],
            metadata: json!({}),
            model: None,
        }, "test")?;
        engine.put_vector("local", NewVector {
            id: Some("orthogonal".into()),
            values: vec![0.0, 1.0],
            metadata: json!({}),
            model: None,
        }, "test")?;
        let hits = engine.search_vectors("local", &[1.0, 0.0], "cosine", 2)?;
        assert_eq!(hits[0].entry.id, "same");
        assert!(hits[0].score > hits[1].score);
        Ok(())
    }

    #[test]
    fn namespace_validation_rejects_path_like_input() {
        assert!(validate_namespace("../other").is_err());
        assert!(validate_namespace("tenant-01").is_ok());
    }
}
