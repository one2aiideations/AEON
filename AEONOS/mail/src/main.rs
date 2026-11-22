use anyhow::{Context, Result};
use argon2::{password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString}, Argon2};
use axum::{extract::{Path, State}, http::{HeaderMap, StatusCode}, response::IntoResponse, routing::{get, post}, Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{sqlite::{SqliteConnectOptions, SqlitePoolOptions}, SqlitePool};
use std::{net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::{TcpListener, TcpStream}};
use tracing::{error, info, warn};
use uuid::Uuid;

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY,email TEXT NOT NULL UNIQUE,password_hash TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY,user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,token_hash TEXT NOT NULL UNIQUE,expires_at INTEGER NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE INDEX IF NOT EXISTS idx_sessions_token_hash ON sessions(token_hash);
CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY,user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,email TEXT NOT NULL,display_name TEXT NOT NULL DEFAULT '',incoming_server TEXT,outgoing_server TEXT,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,UNIQUE(user_id,email));
CREATE TABLE IF NOT EXISTS mailboxes (id TEXT PRIMARY KEY,account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,name TEXT NOT NULL,uid_validity INTEGER NOT NULL DEFAULT 1,UNIQUE(account_id,name));
CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY,mailbox_id TEXT NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,message_id TEXT,subject TEXT NOT NULL DEFAULT '',sender TEXT NOT NULL DEFAULT '',recipients_json TEXT NOT NULL DEFAULT '[]',text_body TEXT NOT NULL DEFAULT '',html_body TEXT,flags_json TEXT NOT NULL DEFAULT '[]',received_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,UNIQUE(mailbox_id,message_id));
CREATE INDEX IF NOT EXISTS idx_messages_mailbox_received ON messages(mailbox_id,received_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_subject ON messages(subject);
CREATE TABLE IF NOT EXISTS attachments (id TEXT PRIMARY KEY,message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,filename TEXT NOT NULL,content_type TEXT NOT NULL,storage_key TEXT NOT NULL,size_bytes INTEGER NOT NULL CHECK(size_bytes>=0));
CREATE TABLE IF NOT EXISTS agent_identities (id TEXT PRIMARY KEY,user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,name TEXT NOT NULL,credential_hash TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS agent_permissions (agent_id TEXT NOT NULL REFERENCES agent_identities(id) ON DELETE CASCADE,mailbox_id TEXT NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,can_read INTEGER NOT NULL DEFAULT 0 CHECK(can_read IN (0,1)),can_write INTEGER NOT NULL DEFAULT 0 CHECK(can_write IN (0,1)),PRIMARY KEY(agent_id,mailbox_id));
CREATE TABLE IF NOT EXISTS audit_log (id TEXT PRIMARY KEY,actor_type TEXT NOT NULL,actor_id TEXT NOT NULL,action TEXT NOT NULL,resource_type TEXT NOT NULL,resource_id TEXT NOT NULL,detail_json TEXT NOT NULL DEFAULT '{}',created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS delivery_queue (id TEXT PRIMARY KEY,account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,message_id TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'queued',attempts INTEGER NOT NULL DEFAULT 0,next_attempt_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,last_error TEXT);
"#;

#[derive(Clone)]
struct AppState { db: SqlitePool }

#[derive(Parser)]
#[command(name="aeon-mail",version,about="AEON-MAIL monolithic email platform")]
struct Cli {
    #[arg(long,env="AEON_MAIL_DATABASE",default_value="aeon-mail.sqlite")]
    database: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long,env="AEON_MAIL_BIND",default_value="127.0.0.1:8787")]
        bind: SocketAddr,
        #[arg(long,env="AEON_MAIL_SMTP_BIND",default_value="127.0.0.1:2525")]
        smtp_bind: SocketAddr,
    },
    Migrate,
    Status,
}
#[derive(Debug,Serialize)]
struct Health { status: &'static str, service: &'static str, database: &'static str }
#[derive(Debug,Deserialize)]
struct RegisterUser { email:String, password:String }
#[derive(Debug,Deserialize)]
struct LoginUser { email:String, password:String }
#[derive(Debug,Deserialize)]
struct CreateAccount { email:String, display_name:Option<String>, incoming_server:Option<String>, outgoing_server:Option<String> }
#[derive(Debug,Deserialize)]
struct CreateMailbox { account_id:String, name:String }
#[derive(Debug,Serialize)]
struct AuthResponse { user_id:String, email:String, access_token:String, token_type:&'static str, expires_in:i64 }
#[derive(Debug,Serialize)]
struct Mailbox { id:String, account_id:String, name:String, uid_validity:i64 }

fn normalize_email(raw:&str)->Option<String>{
    let email=raw.trim().to_ascii_lowercase();
    if email.len()>254 || email.len()<3 || email.contains(char::is_whitespace){return None}
    let (local,domain)=email.split_once('@')?;
    if local.is_empty() || domain.is_empty() || !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {return None}
    Some(email)
}
fn token_hash(token:&str)->String{format!("{:x}",Sha256::digest(token.as_bytes()))}
fn new_token()->String{format!("{}{}",Uuid::new_v4().simple(),Uuid::new_v4().simple())}

async fn open_database(path:&PathBuf)->Result<SqlitePool>{
    if let Some(parent)=path.parent(){if !parent.as_os_str().is_empty(){tokio::fs::create_dir_all(parent).await?}}
    let options=SqliteConnectOptions::from_str(&format!("sqlite://{}",path.display()))?.create_if_missing(true).foreign_keys(true);
    let pool=SqlitePoolOptions::new().max_connections(5).connect_with(options).await.with_context(||format!("opening database at {}",path.display()))?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await.context("applying schema")?;
    Ok(pool)
}
async fn authenticated_user(headers:&HeaderMap,db:&SqlitePool)->Result<String,(StatusCode,String)>{
    let value=headers.get(axum::http::header::AUTHORIZATION).and_then(|v|v.to_str().ok()).ok_or((StatusCode::UNAUTHORIZED,"Bearer token required".into()))?;
    let token=value.strip_prefix("Bearer ").filter(|v|!v.is_empty()&&v.len()<=128).ok_or((StatusCode::UNAUTHORIZED,"invalid authorization header".into()))?;
    sqlx::query_scalar("SELECT user_id FROM sessions WHERE token_hash=?1 AND expires_at>unixepoch()")
        .bind(token_hash(token)).fetch_optional(db).await
        .map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"session lookup failed".into()))?
        .ok_or((StatusCode::UNAUTHORIZED,"session expired or invalid".into()))
}
async fn issue_session(db:&SqlitePool,user_id:&str,email:String,status:StatusCode)->Result<(StatusCode,Json<AuthResponse>),(StatusCode,String)>{
    let token=new_token();let expires=86400_i64;
    sqlx::query("INSERT INTO sessions(id,user_id,token_hash,expires_at) VALUES(?1,?2,?3,unixepoch()+?4)")
        .bind(Uuid::new_v4().to_string()).bind(user_id).bind(token_hash(&token)).bind(expires).execute(db).await
        .map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"session creation failed".into()))?;
    Ok((status,Json(AuthResponse{user_id:user_id.to_owned(),email,access_token:token,token_type:"Bearer",expires_in:expires})))
}
async fn register(State(s):State<Arc<AppState>>,Json(i):Json<RegisterUser>)->Result<impl IntoResponse,(StatusCode,String)>{
    let email=normalize_email(&i.email).ok_or((StatusCode::BAD_REQUEST,"valid email address required".into()))?;
    if i.password.len()<12||i.password.len()>1024{return Err((StatusCode::BAD_REQUEST,"password must contain 12..=1024 bytes".into()))}
    let salt=SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"password hashing failed".into()))?;
    let hash=Argon2::default().hash_password(i.password.as_bytes(),&salt).map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"password hashing failed".into()))?.to_string();
    let id=Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO users(id,email,password_hash) VALUES(?1,?2,?3)").bind(&id).bind(&email).bind(hash).execute(&s.db).await.map_err(|_|(StatusCode::CONFLICT,"email already registered".into()))?;
    issue_session(&s.db,&id,email,StatusCode::CREATED).await
}
async fn login(State(s):State<Arc<AppState>>,Json(i):Json<LoginUser>)->Result<Json<AuthResponse>,(StatusCode,String)>{
    let email=normalize_email(&i.email).ok_or((StatusCode::BAD_REQUEST,"valid email address required".into()))?;
    let row=sqlx::query_as::<_,(String,String)>("SELECT id,password_hash FROM users WHERE email=?1").bind(&email).fetch_optional(&s.db).await.map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"authentication lookup failed".into()))?.ok_or((StatusCode::UNAUTHORIZED,"invalid email or password".into()))?;
    let parsed=PasswordHash::new(&row.1).map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"stored password hash invalid".into()))?;
    if Argon2::default().verify_password(i.password.as_bytes(),&parsed).is_err(){return Err((StatusCode::UNAUTHORIZED,"invalid email or password".into()))}
    let (_,response)=issue_session(&s.db,&row.0,email,StatusCode::OK).await?;Ok(response)
}
async fn create_account(State(s):State<Arc<AppState>>,headers:HeaderMap,Json(i):Json<CreateAccount>)->Result<impl IntoResponse,(StatusCode,String)>{
    let uid=authenticated_user(&headers,&s.db).await?;
    let email=normalize_email(&i.email).ok_or((StatusCode::BAD_REQUEST,"valid account email required".into()))?;
    let id=Uuid::new_v4().to_string();let display=i.display_name.unwrap_or_default();
    sqlx::query("INSERT INTO accounts(id,user_id,email,display_name,incoming_server,outgoing_server) VALUES(?1,?2,?3,?4,?5,?6)")
        .bind(&id).bind(uid).bind(&email).bind(&display).bind(i.incoming_server).bind(i.outgoing_server).execute(&s.db).await.map_err(|_|(StatusCode::CONFLICT,"account already exists or could not be saved".into()))?;
    Ok((StatusCode::CREATED,Json(serde_json::json!({"id":id,"email":email,"display_name":display}))))
}
async fn create_mailbox(State(s):State<Arc<AppState>>,headers:HeaderMap,Json(i):Json<CreateMailbox>)->Result<impl IntoResponse,(StatusCode,String)>{
    let uid=authenticated_user(&headers,&s.db).await?;let name=i.name.trim();
    if name.is_empty()||name.len()>128{return Err((StatusCode::BAD_REQUEST,"mailbox name must contain 1..=128 characters".into()))}
    let owns:i64=sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE id=?1 AND user_id=?2").bind(&i.account_id).bind(uid).fetch_one(&s.db).await.map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"account check failed".into()))?;
    if owns==0{return Err((StatusCode::NOT_FOUND,"account not found".into()))}
    let id=Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO mailboxes(id,account_id,name) VALUES(?1,?2,?3)").bind(&id).bind(&i.account_id).bind(name).execute(&s.db).await.map_err(|_|(StatusCode::CONFLICT,"mailbox already exists or could not be created".into()))?;
    Ok((StatusCode::CREATED,Json(Mailbox{id,account_id:i.account_id,name:name.to_owned(),uid_validity:1})))
}
async fn list_mailboxes(State(s):State<Arc<AppState>>,headers:HeaderMap,Path(account_id):Path<String>)->Result<Json<Vec<Mailbox>>,(StatusCode,String)>{
    let uid=authenticated_user(&headers,&s.db).await?;
    let rows=sqlx::query_as::<_,(String,String,String,i64)>("SELECT m.id,m.account_id,m.name,m.uid_validity FROM mailboxes m JOIN accounts a ON a.id=m.account_id WHERE m.account_id=?1 AND a.user_id=?2 ORDER BY m.name").bind(account_id).bind(uid).fetch_all(&s.db).await.map_err(|_|(StatusCode::INTERNAL_SERVER_ERROR,"database query failed".into()))?;
    Ok(Json(rows.into_iter().map(|(id,account_id,name,uid_validity)|Mailbox{id,account_id,name,uid_validity}).collect()))
}
async fn health()->Json<Health>{Json(Health{status:"ok",service:"aeon-mail",database:"connected"})}
fn app(s:Arc<AppState>)->Router{
    Router::new().route("/health",get(health)).route("/api/v1/auth/register",post(register)).route("/api/v1/auth/login",post(login))
        .route("/api/v1/accounts",post(create_account)).route("/api/v1/mailboxes",post(create_mailbox))
        .route("/api/v1/accounts/:account_id/mailboxes",get(list_mailboxes)).with_state(s)
}

async fn smtp_reply(stream:&mut TcpStream,line:&str)->Result<()>{stream.write_all(format!("{line}\r\n").as_bytes()).await?;stream.flush().await?;Ok(())}
fn extract_path(command:&str)->Option<String>{
    let start=command.find('<')?+1;let end=command[start..].find('>')?+start;Some(command[start..end].trim().to_ascii_lowercase())
}
async fn smtp_session(stream:TcpStream,db:SqlitePool)->Result<()>{
    let (read,mut write)=stream.into_split();let mut reader=BufReader::new(read);let mut line=String::new();
    write.write_all(b"220 aeon-mail ESMTP ready\r\n").await?;
    let mut user_id:Option<String>=None;let mut sender:Option<String>=None;let mut recipients:Vec<String>=Vec::new();
    loop{
        line.clear();let n=reader.read_line(&mut line).await?;if n==0{break}
        if line.len()>2048{write.write_all(b"500 command too long\r\n").await?;continue}
        let command=line.trim_end_matches(&['\r','\n'][..]).to_owned();let upper=command.to_ascii_uppercase();
        if upper.starts_with("EHLO ")||upper.starts_with("HELO "){write.write_all(b"250-aeon-mail\r\n250-AUTH PLAIN\r\n250 SIZE 10485760\r\n").await?;}
        else if upper=="NOOP"{write.write_all(b"250 OK\r\n").await?;}
        else if upper=="RSET"{sender=None;recipients.clear();write.write_all(b"250 reset\r\n").await?;}
        else if upper=="QUIT"{write.write_all(b"221 bye\r\n").await?;break;}
        else if upper.starts_with("AUTH PLAIN "){
            let encoded=command.split_whitespace().nth(2).unwrap_or("");
            let decoded=BASE64.decode(encoded).unwrap_or_default();let parts:Vec<&[u8]>=decoded.split(|b|*b==0).collect();
            if parts.len()!=3{write.write_all(b"535 authentication failed\r\n").await?;continue}
            let email=String::from_utf8_lossy(parts[1]).to_ascii_lowercase();
            let row=sqlx::query_as::<_,(String,String)>("SELECT id,password_hash FROM users WHERE email=?1").bind(email).fetch_optional(&db).await?;
            let valid=if let Some((id,hash))=row{
                let parsed=PasswordHash::new(&hash).ok();
                if parsed.as_ref().is_some_and(|p|Argon2::default().verify_password(parts[2],p).is_ok()){user_id=Some(id);true}else{false}
            }else{false};
            if valid{write.write_all(b"235 2.7.0 authenticated\r\n").await?;}else{write.write_all(b"535 authentication failed\r\n").await?;}
        }
        else if upper.starts_with("MAIL FROM:"){
            let Some(uid)=user_id.as_ref() else{write.write_all(b"530 authenticate first\r\n").await?;continue};
            let Some(address)=extract_path(&command) else{write.write_all(b"501 invalid sender\r\n").await?;continue};
            let allowed:i64=sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE user_id=?1 AND email=?2").bind(uid).bind(&address).fetch_one(&db).await?;
            if allowed==0{write.write_all(b"553 sender address is not owned by authenticated user\r\n").await?;continue}
            sender=Some(address);recipients.clear();write.write_all(b"250 sender accepted\r\n").await?;
        }
        else if upper.starts_with("RCPT TO:"){
            if sender.is_none(){write.write_all(b"503 send MAIL FROM first\r\n").await?;continue}
            let Some(address)=extract_path(&command) else{write.write_all(b"501 invalid recipient\r\n").await?;continue};
            let exists:i64=sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE email=?1").bind(&address).fetch_one(&db).await?;
            if exists==0{write.write_all(b"550 recipient not hosted here\r\n").await?;continue}
            if recipients.len()>=50{write.write_all(b"452 too many recipients\r\n").await?;continue}
            recipients.push(address);write.write_all(b"250 recipient accepted\r\n").await?;
        }
        else if upper=="DATA"{
            if sender.is_none()||recipients.is_empty(){write.write_all(b"503 sender and recipient required\r\n").await?;continue}
            write.write_all(b"354 end with <CRLF>.<CRLF>\r\n").await?;
            let mut raw=String::new();let mut total=0usize;let mut too_large=false;
            loop{line.clear();let n=reader.read_line(&mut line).await?;if n==0{break}
                if line==".\r\n"{break}
                total+=n;if total>10*1024*1024{too_large=true}
                if !too_large{if line.starts_with(".."){line.remove(0);}raw.push_str(&line);}
            }
            if too_large{write.write_all(b"552 message exceeds 10 MiB\r\n").await?;sender=None;recipients.clear();continue}
            let (headers,body)=raw.split_once("\r\n\r\n").or_else(||raw.split_once("\n\n")).unwrap_or((&raw,""));
            let mut subject=String::new();let mut message_id=format!("<{}@aeon-mail.local>",Uuid::new_v4());
            for h in headers.lines(){if let Some(v)=h.strip_prefix("Subject:").or_else(||h.strip_prefix("subject:")){subject=v.trim().to_owned();}
                if let Some(v)=h.strip_prefix("Message-ID:").or_else(||h.strip_prefix("Message-Id:")){message_id=v.trim().to_owned();}}
            let sender_addr=sender.clone().unwrap_or_default();let rcpts=serde_json::to_string(&recipients)?;
            let mut delivered=0usize;
            for recipient in &recipients{
                if let Some((account_id,))=sqlx::query_as::<_,(String,)>("SELECT id FROM accounts WHERE email=?1").bind(recipient).fetch_optional(&db).await?{
                    let inbox_id=Uuid::new_v4().to_string();
                    sqlx::query("INSERT INTO mailboxes(id,account_id,name) VALUES(?1,?2,'Inbox') ON CONFLICT(account_id,name) DO NOTHING").bind(&inbox_id).bind(&account_id).execute(&db).await?;
                    let actual_inbox:String=sqlx::query_scalar("SELECT id FROM mailboxes WHERE account_id=?1 AND name='Inbox'").bind(&account_id).fetch_one(&db).await?;
                    let msg_id=Uuid::new_v4().to_string();
                    sqlx::query("INSERT OR IGNORE INTO messages(id,mailbox_id,message_id,subject,sender,recipients_json,text_body) VALUES(?1,?2,?3,?4,?5,?6,?7)")
                        .bind(&msg_id).bind(&actual_inbox).bind(&message_id).bind(&subject).bind(&sender_addr).bind(&rcpts).bind(body).execute(&db).await?;
                    delivered+=1;
                }
            }
            if delivered>0{write.write_all(format!("250 queued for local delivery to {delivered} recipient(s)\r\n").as_bytes()).await?;}
            else{write.write_all(b"550 no valid local recipients\r\n").await?;}
            sender=None;recipients.clear();
        }
        else{write.write_all(b"502 command not implemented\r\n").await?;}
    }
    Ok(())
}
async fn run_smtp(bind:SocketAddr,db:SqlitePool)->Result<()>{
    if !bind.ip().is_loopback(){warn!(address=%bind,"SMTP listener is not loopback; plaintext AUTH is unsafe. Do not expose this listener to untrusted networks.");}
    let listener=TcpListener::bind(bind).await?;info!(address=%listener.local_addr()?,"AEON-MAIL local SMTP submission listener started");
    loop{let (stream,peer)=listener.accept().await?;let pool=db.clone();tokio::spawn(async move{if let Err(e)=smtp_session(stream,pool).await{error!(peer=%peer,error=%e,"SMTP session failed");}});}
}
#[tokio::main]
async fn main()->Result<()>{
    tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_|"info".into())).init();
    let cli=Cli::parse();let db=open_database(&cli.database).await?;
    match cli.command.unwrap_or(Command::Serve{bind:"127.0.0.1:8787".parse()?,smtp_bind:"127.0.0.1:2525".parse()?}){
        Command::Migrate=>info!(database=%cli.database.display(),"database schema is up to date"),
        Command::Status=>{sqlx::query("SELECT 1").execute(&db).await?;println!("AEON-MAIL database: healthy");}
        Command::Serve{bind,smtp_bind}=>{
            let state=Arc::new(AppState{db:db.clone()});let listener=TcpListener::bind(bind).await?;
            info!(address=%listener.local_addr()?,"AEON-MAIL HTTP API listening");
            tokio::spawn(async move{if let Err(e)=run_smtp(smtp_bind,db).await{error!(error=%e,"SMTP listener stopped");}});
            axum::serve(listener,app(state)).await?;
        }
    }Ok(())
}
#[cfg(test)]
mod tests{
    use super::*;
    async fn db()->SqlitePool{let p=SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();sqlx::raw_sql(SCHEMA).execute(&p).await.unwrap();p}
    #[tokio::test]async fn schema_has_core_tables(){let p=db().await;let n:i64=sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('users','sessions','accounts','mailboxes','messages','attachments','agent_identities','agent_permissions','audit_log','delivery_queue')").fetch_one(&p).await.unwrap();assert_eq!(n,10);}
    #[tokio::test]async fn mailbox_rejects_missing_account(){let p=db().await;assert!(sqlx::query("INSERT INTO mailboxes(id,account_id,name) VALUES('m','missing','Inbox')").execute(&p).await.is_err());}
    #[tokio::test]async fn mailbox_unique_per_account(){let p=db().await;sqlx::query("INSERT INTO users(id,email,password_hash) VALUES('u','u@example.test','hash')").execute(&p).await.unwrap();sqlx::query("INSERT INTO accounts(id,user_id,email) VALUES('a','u','u@example.test')").execute(&p).await.unwrap();sqlx::query("INSERT INTO mailboxes(id,account_id,name) VALUES('m1','a','Inbox')").execute(&p).await.unwrap();assert!(sqlx::query("INSERT INTO mailboxes(id,account_id,name) VALUES('m2','a','Inbox')").execute(&p).await.is_err());}
    #[test]fn email_normalization(){assert_eq!(normalize_email(" A@Example.com ").as_deref(),Some("a@example.com"));assert!(normalize_email("not-an-email").is_none());}
    #[test]fn smtp_path_parser(){assert_eq!(extract_path("RCPT TO:<User@Example.com>"),Some("user@example.com".into()));assert!(extract_path("RCPT TO:invalid").is_none());}
}
