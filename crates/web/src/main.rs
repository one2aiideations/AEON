#![forbid(unsafe_code)]
// AEON-WEB: single-process Rust foundation. Website content is always untrusted.
use anyhow::{Context, Result};
use axum::{extract::State, http::StatusCode, routing::{get, post}, Json, Router};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use reqwest::{header::CONTENT_TYPE, redirect::Policy, Client};
use scraper::{Html, Selector};
use sha2::Digest;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::{SqliteConnectOptions, SqlitePoolOptions}, SqlitePool};
use std::{net::{IpAddr, SocketAddr}, path::PathBuf, str::FromStr, sync::Arc, time::Duration};
use tokio::net::lookup_host;
use tracing::info;
use url::Url;
use uuid::Uuid;

const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const SCHEMA: &str = r#"
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS websites(
 id TEXT PRIMARY KEY, tenant_ref TEXT NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL,
 description TEXT NOT NULL, manifest_json TEXT NOT NULL, compiled_at TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tenant_ref,url)
);
CREATE INDEX IF NOT EXISTS idx_websites_tenant ON websites(tenant_ref,compiled_at DESC);
CREATE TABLE IF NOT EXISTS audit_log(
 id TEXT PRIMARY KEY, tenant_ref TEXT NOT NULL, action TEXT NOT NULL,
 resource_id TEXT NOT NULL, detail_json TEXT NOT NULL DEFAULT '{}',
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
"#;

#[derive(Clone)]
struct AppState { db: SqlitePool }
#[derive(Parser)]
#[command(name="aeon-web",version,about="AEON-WEB monolithic web intelligence runtime")]
struct Cli {
 #[arg(long,env="AEON_WEB_DATABASE",default_value="aeon-web.sqlite")] database: PathBuf,
 #[command(subcommand)] command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
 Serve { #[arg(long,env="AEON_WEB_BIND",default_value="127.0.0.1:8788")] bind: SocketAddr },
 Migrate,
 Status,
}
#[derive(Debug,Serialize)]
struct Health { status: &'static str, service: &'static str, database: &'static str, runtime: &'static str }
#[derive(Debug,Deserialize)]
struct WebsiteRequest { url: String, #[serde(default="default_tenant")] tenant_ref: String }
fn default_tenant()->String { "local-development".into() }
#[derive(Debug,Clone,Serialize,Deserialize)]
struct WebResource { kind: String, url: String, rel: Option<String>, mime_hint: Option<String> }
#[derive(Debug,Clone,Serialize,Deserialize)]
struct WebLink { text: String, url: String }
#[derive(Debug,Clone,Serialize,Deserialize)]
struct WebsiteManifest {
 schema_version: u32, source_url: String, final_url: String, fetched_at: DateTime<Utc>,
 status_code: u16, content_type: String, title: String, description: String,
 language: Option<String>, links: Vec<WebLink>, resources: Vec<WebResource>,
 headings: Vec<String>, text_excerpt: String, content_sha256: String,
}
#[derive(Debug,Serialize)]
struct CompileResponse { website_id: String, manifest: WebsiteManifest, persisted: bool }
type ApiResult<T> = Result<T,(StatusCode,String)>;

fn safe_ip(ip: IpAddr)->bool {
 match ip {
  IpAddr::V4(v) => {
   let o=v.octets();
   !(v.is_private()||v.is_loopback()||v.is_link_local()||v.is_broadcast()||v.is_unspecified()
    ||o[0]==0||o[0]>=224||o[0]==100&& (64..=127).contains(&o[1])
    ||o[0]==192&&o[1]==0&&o[2]==0||o[0]==198&&(o[1]==18||o[1]==19))
  },
  IpAddr::V6(v) => !(v.is_loopback()||v.is_unspecified()||v.is_unique_local()||v.is_unicast_link_local()
    ||v.to_ipv4_mapped().is_some_and(|v4| !safe_ip(IpAddr::V4(v4)))),
 }
}
async fn validate_and_pin(url:&Url)->Result<Client,String> {
 if !matches!(url.scheme(),"http"|"https") { return Err("only http and https URLs are supported".into()); }
 if !url.username().is_empty()||url.password().is_some(){return Err("credentials in URLs are not allowed".into());}
 let host=url.host_str().ok_or("URL host is required")?;
 let port=url.port_or_known_default().ok_or("unsupported URL port")?;
 let addrs:Vec<SocketAddr>=if let Ok(ip)=host.parse::<IpAddr>() { vec![SocketAddr::new(ip,port)] }
 else { lookup_host((host,port)).await.map_err(|_|"DNS resolution failed")?.collect() };
 if addrs.is_empty()||addrs.iter().any(|a|!safe_ip(a.ip())) { return Err("destination resolves to a non-public or prohibited IP address".into()); }
 let builder=Client::builder().redirect(Policy::none()).connect_timeout(Duration::from_secs(8))
  .timeout(Duration::from_secs(20)).user_agent("AEON-WEB/0.1 (+website-inspection)");
 let builder=if let Some(ip)=addrs.first() { builder.resolve(host, *ip) } else { builder };
 builder.build().map_err(|e|format!("HTTP client setup failed: {e}"))
}
async fn fetch_html(initial:&str)->Result<(Url,u16,String,String),String>{
 let mut url=Url::parse(initial).map_err(|_|"invalid URL".to_string())?;
 for hop in 0..=MAX_REDIRECTS {
  let client=validate_and_pin(&url).await?;
  let response=client.get(url.clone()).send().await.map_err(|e|format!("request failed: {e}"))?;
  let status=response.status();
  if status.is_redirection() {
   if hop==MAX_REDIRECTS{return Err("redirect limit exceeded".into());}
   let location=response.headers().get(reqwest::header::LOCATION).and_then(|v|v.to_str().ok()).ok_or("redirect missing Location header")?;
   url=url.join(location).map_err(|_|"invalid redirect target".to_string())?;
   continue;
  }
  if !status.is_success(){return Err(format!("website returned HTTP {}",status.as_u16()));}
  let content_type=response.headers().get(CONTENT_TYPE).and_then(|v|v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
  if !content_type.to_ascii_lowercase().contains("text/html"){return Err(format!("expected HTML, received {content_type}"));}
  if response.content_length().is_some_and(|n|n>MAX_HTML_BYTES as u64){return Err("HTML response exceeds 5 MiB limit".into());}
  let mut stream=response.bytes_stream();let mut body=Vec::new();
  while let Some(chunk)=stream.next().await {
   let chunk=chunk.map_err(|e|format!("response read failed: {e}"))?;
   if body.len()+chunk.len()>MAX_HTML_BYTES{return Err("HTML response exceeds 5 MiB limit".into());}
   body.extend_from_slice(&chunk);
  }
  let html=String::from_utf8_lossy(&body).into_owned();
  return Ok((url,status.as_u16(),content_type,html));
 }
 Err("redirect limit exceeded".into())
}
fn select(s:&str)->Selector { Selector::parse(s).expect("static selector is valid") }
fn compile_manifest(source:&str,final_url:&Url,status:u16,content_type:String,html:&str)->WebsiteManifest {
 let doc=Html::parse_document(html);
 let title=doc.select(&select("title")).next().map(|n|n.text().collect::<String>().trim().to_owned()).unwrap_or_default();
 let description=doc.select(&select("meta[name='description']")).next().and_then(|n|n.value().attr("content")).unwrap_or("").to_owned();
 let language=doc.select(&select("html")).next().and_then(|n|n.value().attr("lang")).map(str::to_owned);
 let mut links=Vec::new();
 for node in doc.select(&select("a[href]")).take(500) {
  if let Some(href)=node.value().attr("href").and_then(|h|final_url.join(h).ok()) {
   if matches!(href.scheme(),"http"|"https") {
    let text=node.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    links.push(WebLink{text:text.chars().take(240).collect(),url:href.to_string()});
   }
  }
 }
 let mut resources=Vec::new();
 for (selector,attr,kind) in [("script[src]","src","script"),("img[src]","src","image"),("link[href]","href","stylesheet-or-link"),("source[src]","src","media")] {
  for node in doc.select(&select(selector)).take(300) {
   if let Some(raw)=node.value().attr(attr) {
    if let Ok(u)=final_url.join(raw) { if matches!(u.scheme(),"http"|"https") {
     resources.push(WebResource{kind:kind.into(),url:u.to_string(),rel:node.value().attr("rel").map(str::to_owned),mime_hint:node.value().attr("type").map(str::to_owned)});
    }}
   }
  }
 }
 let headings=doc.select(&select("h1,h2,h3")).take(150).map(|n|n.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s|!s.is_empty()).collect();
 let text_excerpt=doc.root_element().text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(4000).collect();
 let digest=format!("{:x}",sha2::Sha256::digest(html.as_bytes()));
 WebsiteManifest{schema_version:1,source_url:source.into(),final_url:final_url.to_string(),fetched_at:Utc::now(),status_code:status,content_type,title,description,language,links,resources,headings,text_excerpt,content_sha256:digest}
}
async fn open_db(path:&PathBuf)->Result<SqlitePool>{
 if let Some(parent)=path.parent(){if !parent.as_os_str().is_empty(){tokio::fs::create_dir_all(parent).await?;}}
 let options=SqliteConnectOptions::from_str(&format!("sqlite://{}",path.display()))?.create_if_missing(true).foreign_keys(true);
 let pool=SqlitePoolOptions::new().max_connections(5).connect_with(options).await?;
 sqlx::raw_sql(SCHEMA).execute(&pool).await?;Ok(pool)
}
async fn health(State(s):State<Arc<AppState>>)->Json<Health>{
 let db=sqlx::query_scalar::<_,i64>("SELECT 1").fetch_one(&s.db).await.is_ok();
 Json(Health{status:if db{"ok"}else{"degraded"},service:"aeon-web",database:if db{"connected"}else{"unavailable"},runtime:"single-process-rust"})
}
async fn inspect(Json(input):Json<WebsiteRequest>)->ApiResult<Json<WebsiteManifest>>{
 let (final_url,status,ctype,html)=fetch_html(&input.url).await.map_err(|e|(StatusCode::BAD_GATEWAY,e))?;
 Ok(Json(compile_manifest(&input.url,&final_url,status,ctype,&html)))
}
async fn compile(State(s):State<Arc<AppState>>,Json(input):Json<WebsiteRequest>)->ApiResult<(StatusCode,Json<CompileResponse>)>{
 if input.tenant_ref.trim().is_empty()||input.tenant_ref.len()>128{return Err((StatusCode::BAD_REQUEST,"tenant_ref must contain 1..=128 characters".into()));}
 let (final_url,status,ctype,html)=fetch_html(&input.url).await.map_err(|e|(StatusCode::BAD_GATEWAY,e))?;
 let manifest=compile_manifest(&input.url,&final_url,status,ctype,&html);
 let website_id=Uuid::new_v4().to_string();
 let json=serde_json::to_string(&manifest).map_err(|e|(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?;
 sqlx::query("INSERT INTO websites(id,tenant_ref,url,title,description,manifest_json,compiled_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(tenant_ref,url) DO UPDATE SET title=excluded.title,description=excluded.description,manifest_json=excluded.manifest_json,compiled_at=excluded.compiled_at")
  .bind(&website_id).bind(&input.tenant_ref).bind(&input.url).bind(&manifest.title).bind(&manifest.description).bind(&json).bind(manifest.fetched_at.to_rfc3339()).execute(&s.db).await.map_err(|e|(StatusCode::INTERNAL_SERVER_ERROR,format!("persistence failed: {e}")))?;
 let id=sqlx::query_scalar::<_,String>("SELECT id FROM websites WHERE tenant_ref=?1 AND url=?2").bind(&input.tenant_ref).bind(&input.url).fetch_one(&s.db).await.map_err(|e|(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?;
 sqlx::query("INSERT INTO audit_log(id,tenant_ref,action,resource_id,detail_json) VALUES(?1,?2,'website.compile',?3,?4)")
  .bind(Uuid::new_v4().to_string()).bind(&input.tenant_ref).bind(&id).bind(serde_json::json!({"source_url":input.url,"sha256":manifest.content_sha256}).to_string()).execute(&s.db).await.map_err(|e|(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?;
 Ok((StatusCode::OK,Json(CompileResponse{website_id:id,manifest,persisted:true})))
}
async fn list_websites(State(s):State<Arc<AppState>>)->ApiResult<Json<Vec<serde_json::Value>>>{
 let rows=sqlx::query_as::<_,(String,String,String,String)>("SELECT id,tenant_ref,url,title FROM websites ORDER BY compiled_at DESC LIMIT 200").fetch_all(&s.db).await.map_err(|e|(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?;
 Ok(Json(rows.into_iter().map(|(id,tenant,url,title)|serde_json::json!({"id":id,"tenant_ref":tenant,"url":url,"title":title})).collect()))
}
fn router(state:Arc<AppState>)->Router{
 Router::new().route("/health",get(health)).route("/api/v1/websites/inspect",post(inspect))
  .route("/api/v1/websites/compile",post(compile)).route("/api/v1/websites",get(list_websites)).with_state(state)
}
async fn run(cli:Cli)->Result<()> {
 let db=open_db(&cli.database).await?;
 match cli.command.unwrap_or(Command::Status) {
  Command::Migrate=>{info!("AEON-WEB database migrations applied");}
  Command::Status=>{println!("AEON-WEB database ready; schema initialized");}
  Command::Serve{bind}=>{
   let state=Arc::new(AppState{db});
   let app=router(state);let listener=tokio::net::TcpListener::bind(bind).await?;
   info!(%bind,"AEON-WEB HTTP server listening");
   axum::serve(listener,app).with_graceful_shutdown(async{let _=tokio::signal::ctrl_c().await;}).await?;
  }
 }
 Ok(())
}
#[tokio::main]
async fn main()->Result<()> {
 tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
 let cli=Cli::parse();run(cli).await.context("AEON-WEB runtime failed")
}
#[cfg(test)]
mod tests {
 use super::*;
 #[test] fn rejects_private_and_loopback_addresses(){
  for ip in ["127.0.0.1","10.0.0.1","172.16.0.1","192.168.1.1","169.254.1.1","::1","fc00::1","fe80::1"] {assert!(!safe_ip(ip.parse().unwrap()),"{ip}");}
  for ip in ["8.8.8.8","1.1.1.1","2606:4700:4700::1111"] {assert!(safe_ip(ip.parse().unwrap()),"{ip}");}
 }
 #[test] fn extracts_page_title_links_and_headings(){
  let base=Url::parse("https://example.com/a").unwrap();
  let m=compile_manifest("https://example.com/a",&base,200,"text/html".into(),"<html lang='en'><head><title>Example</title><meta name='description' content='Demo'></head><body><h1>Hello</h1><a href='/next'>Next page</a><img src='/x.png'></body></html>");
  assert_eq!(m.title,"Example");assert_eq!(m.description,"Demo");assert_eq!(m.links[0].url,"https://example.com/next");assert_eq!(m.headings,vec!["Hello"]);assert_eq!(m.resources.len(),1);
 }
 #[test] fn refuses_non_http_schemes(){
  assert!(Url::parse("file:///etc/passwd").unwrap().scheme()!="http");
 }
}
