//! Fake X4 that speaks either firmware dialect over a tempdir "SD card".
//! Behavior mirrors crosspoint-reader's CrossPointWebServer.cpp and the stock
//! calls used by send-to-x4; quirks that were never observed on hardware are not faked.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Form, Multipart, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tempfile::TempDir;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Stock,
    CrossPoint,
}

pub struct Fake {
    pub host: String,
    pub ws_port: u16,
    pub root: TempDir,
    /// WebSocket uploads still to break off after 64 KB, leaving the partial file behind
    /// (worse than the firmware, which deletes it when it notices the disconnect).
    pub drop_uploads: Arc<AtomicUsize>,
    _rt: tokio::runtime::Runtime,
}

type Reply = (StatusCode, String);

fn ok(msg: impl Into<String>) -> Reply {
    (StatusCode::OK, msg.into())
}

fn err(code: u16, msg: impl Into<String>) -> Reply {
    (StatusCode::from_u16(code).unwrap(), msg.into())
}

// ESP32 arduinoWebSockets drops frames above WEBSOCKETS_MAX_DATA_SIZE (15 KB).
const MAX_WS_FRAME: usize = 15 * 1024;

pub fn start(kind: Kind) -> Fake {
    let root = tempfile::tempdir().unwrap();
    let sd = root.path().to_path_buf();
    let drop_uploads = Arc::new(AtomicUsize::new(0));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ws = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let host = http.local_addr().unwrap().to_string();
    let ws_port = ws.local_addr().unwrap().port();
    http.set_nonblocking(true).unwrap();
    ws.set_nonblocking(true).unwrap();

    let app = match kind {
        Kind::CrossPoint => Router::new()
            .route("/api/status", get(cp_status))
            .route("/api/files", get(cp_files))
            .route("/download", get(cp_download))
            .route("/upload", post(cp_upload))
            .route("/mkdir", post(cp_mkdir))
            .route("/rename", post(cp_rename))
            .route("/move", post(cp_move))
            .route("/delete", post(cp_delete)),
        Kind::Stock => Router::new().route("/list", get(stock_list)).route(
            "/edit",
            post(stock_upload).put(stock_mkdir).delete(stock_delete),
        ),
    }
    .with_state(sd.clone());

    rt.spawn(async move {
        let l = tokio::net::TcpListener::from_std(http).unwrap();
        axum::serve(l, app).await.unwrap();
    });
    if kind == Kind::CrossPoint {
        let ws_app = Router::new()
            .route("/", get(ws_upgrade))
            .with_state((sd, drop_uploads.clone()));
        rt.spawn(async move {
            let l = tokio::net::TcpListener::from_std(ws).unwrap();
            axum::serve(l, ws_app).await.unwrap();
        });
    }
    Fake {
        host,
        ws_port,
        root,
        drop_uploads,
        _rt: rt,
    }
}

impl Fake {
    pub fn path(&self, device_path: &str) -> PathBuf {
        disk(self.root.path(), device_path)
    }

    pub fn put(&self, device_path: &str, data: &[u8]) {
        let p = self.path(device_path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    /// Every file and folder on the card, one per line, for test artifacts.
    pub fn tree(&self) -> String {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
                if e.file_type().unwrap().is_dir() {
                    out.push(format!("{name}/"));
                    walk(&e.path(), &name, out);
                } else {
                    out.push(format!("{name}  ({} bytes)", e.metadata().unwrap().len()));
                }
            }
        }
        let mut out = Vec::new();
        walk(self.root.path(), "", &mut out);
        out.join("\n")
    }
}

fn normalize(p: &str) -> String {
    let parts: Vec<&str> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    format!("/{}", parts.join("/"))
}

fn disk(root: &Path, device_path: &str) -> PathBuf {
    root.join(normalize(device_path).trim_start_matches('/'))
}

fn listing(root: &Path, dir: &str) -> Vec<(String, u64, bool)> {
    let Ok(rd) = fs::read_dir(disk(root, dir)) else {
        return vec![];
    };
    rd.map(|e| e.unwrap())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| {
            let md = e.metadata().unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                if md.is_dir() { 0 } else { md.len() },
                md.is_dir(),
            )
        })
        .collect()
}

// ---------- CrossPoint ----------

async fn cp_status() -> Response {
    axum::Json(serde_json::json!({
        "version": "1.2.0-fake", "ip": "127.0.0.1", "mode": "STA", "rssi": -50,
        "freeHeap": 123456, "uptime": 42, "device": "X4", "serial": "Not found"
    }))
    .into_response()
}

async fn cp_files(State(sd): State<PathBuf>, Query(q): Query<HashMap<String, String>>) -> Response {
    let dir = q.get("path").map(String::as_str).unwrap_or("/");
    let items: Vec<_> = listing(&sd, dir)
        .into_iter()
        .map(|(name, size, is_dir)| {
            let epub = name.to_lowercase().ends_with(".epub");
            serde_json::json!({"name": name, "size": size, "isDirectory": is_dir, "isEpub": epub})
        })
        .collect();
    axum::Json(items).into_response()
}

async fn cp_download(
    State(sd): State<PathBuf>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(path) = q.get("path") else {
        return err(400, "Missing path").into_response();
    };
    let p = disk(&sd, path);
    if !p.exists() {
        return err(404, "Item not found").into_response();
    }
    if p.is_dir() {
        return err(400, "Path is a directory").into_response();
    }
    fs::read(p).unwrap().into_response()
}

async fn cp_upload(
    State(sd): State<PathBuf>,
    Query(q): Query<HashMap<String, String>>,
    mut form: Multipart,
) -> Reply {
    let dir = q.get("path").map(String::as_str).unwrap_or("/");
    if let Some(field) = form.next_field().await.unwrap() {
        let name = field.file_name().unwrap_or_default().to_string();
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            return err(400, "Invalid file name");
        }
        let target = disk(&sd, &format!("{dir}/{name}"));
        if target.exists() {
            return err(400, format!("File already exists: {name}"));
        }
        fs::write(target, field.bytes().await.unwrap()).unwrap();
        return ok(format!("File uploaded successfully: {name}"));
    }
    err(400, "Unknown error during upload")
}

#[derive(Deserialize)]
struct MkdirForm {
    name: String,
    path: Option<String>,
}

async fn cp_mkdir(State(sd): State<PathBuf>, Form(f): Form<MkdirForm>) -> Reply {
    let p = disk(
        &sd,
        &format!("{}/{}", f.path.as_deref().unwrap_or("/"), f.name),
    );
    if p.exists() {
        return err(400, "Folder already exists");
    }
    match fs::create_dir(&p) {
        Ok(()) => ok(format!("Folder created: {}", f.name)),
        Err(_) => err(500, "Failed to create folder"),
    }
}

#[derive(Deserialize)]
struct RenameForm {
    path: String,
    name: String,
}

async fn cp_rename(State(sd): State<PathBuf>, Form(f): Form<RenameForm>) -> Reply {
    if f.name.contains('/') {
        return err(400, "Invalid file name");
    }
    let src = disk(&sd, &f.path);
    if !src.exists() {
        return err(404, "Item not found");
    }
    if src.is_dir() {
        return err(400, "Only files can be renamed");
    }
    let dst = src.with_file_name(&f.name);
    if dst.exists() {
        return err(409, "Target already exists");
    }
    fs::rename(src, dst).unwrap();
    ok("Renamed successfully")
}

#[derive(Deserialize)]
struct MoveForm {
    path: String,
    dest: String,
}

async fn cp_move(State(sd): State<PathBuf>, Form(f): Form<MoveForm>) -> Reply {
    let src = disk(&sd, &f.path);
    if !src.exists() {
        return err(404, "Item not found");
    }
    if src.is_dir() {
        return err(400, "Only files can be moved");
    }
    let dest = disk(&sd, &f.dest);
    if !dest.exists() {
        return err(404, "Destination not found");
    }
    if !dest.is_dir() {
        return err(400, "Destination is not a folder");
    }
    let dst = dest.join(src.file_name().unwrap());
    if dst == src {
        return ok("Already in destination");
    }
    if dst.exists() {
        return err(409, "Target already exists");
    }
    fs::rename(src, dst).unwrap();
    ok("Moved successfully")
}

#[derive(Deserialize)]
struct DeleteForm {
    path: Option<String>,
    paths: Option<String>,
}

// Items are deleted in array order; a folder must already be empty when its turn comes.
async fn cp_delete(State(sd): State<PathBuf>, Form(f): Form<DeleteForm>) -> Reply {
    let paths: Vec<String> = match (f.path, f.paths) {
        (Some(p), None) => vec![p],
        (None, Some(json)) => match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(_) => return err(400, "Invalid paths format"),
        },
        (None, None) => return err(400, "Missing `path` or `paths` argument"),
        _ => return err(400, "Provide either 'path' or 'paths', not both"),
    };
    let mut failed = String::new();
    for p in paths {
        let item = normalize(&p);
        let disk_path = disk(&sd, &item);
        if item == "/" {
            failed += &format!("{item} (cannot delete root); ");
        } else if !disk_path.exists() {
            failed += &format!("{item} (not found); ");
        } else if disk_path.is_dir() {
            if fs::read_dir(&disk_path).unwrap().next().is_some() {
                failed += &format!("{item} (folder not empty); ");
            } else {
                fs::remove_dir(&disk_path).unwrap();
            }
        } else {
            fs::remove_file(&disk_path).unwrap();
        }
    }
    if failed.is_empty() {
        ok("All items deleted successfully")
    } else {
        err(500, format!("Failed to delete some items: {failed}"))
    }
}

async fn ws_upgrade(
    State((sd, drops)): State<(PathBuf, Arc<AtomicUsize>)>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| ws_session(socket, sd, drops))
}

async fn ws_session(mut socket: WebSocket, sd: PathBuf, drops: Arc<AtomicUsize>) {
    let mut upload: Option<(PathBuf, usize, Vec<u8>)> = None;
    let mut last_progress = 0;
    while let Some(Ok(msg)) = socket.recv().await {
        let reply = match msg {
            Message::Text(t) => {
                let Some(rest) = t.strip_prefix("START:") else {
                    continue;
                };
                if upload.is_some() {
                    Some("ERROR:Upload already in progress".to_string())
                } else {
                    // Same split as the firmware: name up to the first ':', size to the next, path is the rest.
                    let mut parts = rest.splitn(3, ':');
                    let (name, size, dir) = (parts.next(), parts.next(), parts.next());
                    match (name, size.and_then(|s| s.parse::<usize>().ok()), dir) {
                        (Some(name), Some(size), Some(dir))
                            if !name.is_empty() && !name.contains('/') =>
                        {
                            let target = disk(&sd, &format!("{dir}/{name}"));
                            if target.exists() {
                                Some(format!("ERROR:File already exists: {name}"))
                            } else if size == 0 {
                                fs::write(&target, b"").unwrap();
                                Some("DONE".into())
                            } else {
                                upload = Some((target, size, Vec::new()));
                                last_progress = 0;
                                Some("READY".into())
                            }
                        }
                        _ => Some("ERROR:Invalid START format".into()),
                    }
                }
            }
            Message::Binary(b) => match upload.as_mut() {
                None => Some("ERROR:No upload in progress".into()),
                Some(_) if b.len() > MAX_WS_FRAME => {
                    upload = None;
                    Some("ERROR:Frame too large".into())
                }
                Some((target, size, buf)) => {
                    if buf.len() + b.len() > *size {
                        upload = None;
                        Some("ERROR:Upload overflow".into())
                    } else {
                        buf.extend_from_slice(&b);
                        if buf.len() >= 65536
                            && drops
                                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                                    n.checked_sub(1)
                                })
                                .is_ok()
                        {
                            fs::write(target, &buf).unwrap();
                            return;
                        }
                        let done = buf.len() >= *size;
                        if done || buf.len() - last_progress >= 65536 {
                            last_progress = buf.len();
                            let _ = socket
                                .send(Message::Text(
                                    format!("PROGRESS:{}:{}", buf.len(), size).into(),
                                ))
                                .await;
                        }
                        if done {
                            fs::write(target, &buf).unwrap();
                            upload = None;
                            Some("DONE".into())
                        } else {
                            None
                        }
                    }
                }
            },
            Message::Close(_) => break,
            _ => None,
        };
        if let Some(r) = reply
            && socket.send(Message::Text(r.into())).await.is_err()
        {
            break;
        }
    }
}

// ---------- Stock ----------

// Stock firmware returns some names URL-encoded; the fake encodes names with spaces.
fn encode_name(name: &str) -> String {
    if !name.contains(' ') {
        return name.to_string();
    }
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn stock_list(
    State(sd): State<PathBuf>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let dir = q.get("dir").map(String::as_str).unwrap_or("/");
    if !disk(&sd, dir).is_dir() {
        return err(404, "Not found").into_response();
    }
    let items: Vec<_> = listing(&sd, dir)
        .into_iter()
        .map(|(name, size, is_dir)| {
            serde_json::json!({"type": if is_dir { "dir" } else { "file" }, "name": encode_name(&name), "size": size})
        })
        .collect();
    axum::Json(items).into_response()
}

async fn stock_upload(State(sd): State<PathBuf>, mut form: Multipart) -> Reply {
    while let Some(field) = form.next_field().await.unwrap() {
        if field.name() != Some("data") {
            continue;
        }
        let full = field.file_name().unwrap_or_default().to_string();
        if !full.starts_with('/') {
            return err(500, "BAD PATH");
        }
        let target = disk(&sd, &full);
        if !target.parent().unwrap().is_dir() {
            return err(500, "CREATE FAILED");
        }
        fs::write(target, field.bytes().await.unwrap()).unwrap();
        return ok("");
    }
    err(500, "BAD ARGS")
}

async fn stock_mkdir(State(sd): State<PathBuf>, mut form: Multipart) -> Reply {
    while let Some(field) = form.next_field().await.unwrap() {
        if field.name() == Some("path") {
            let path = field.text().await.unwrap();
            let target = disk(&sd, &path);
            if target.exists() {
                return err(500, "FILE EXISTS");
            }
            return match fs::create_dir(target) {
                Ok(()) => ok(""),
                Err(_) => err(500, "CREATE FAILED"),
            };
        }
    }
    err(500, "BAD ARGS")
}

#[derive(Deserialize)]
struct StockDelete {
    path: String,
}

async fn stock_delete(State(sd): State<PathBuf>, Form(f): Form<StockDelete>) -> Reply {
    let target = disk(&sd, &f.path);
    if normalize(&f.path) == "/" || !target.exists() {
        return err(404, "FileNotFound");
    }
    let res = if target.is_dir() {
        fs::remove_dir(target)
    } else {
        fs::remove_file(target)
    };
    match res {
        Ok(()) => ok(""),
        Err(_) => err(500, "DELETE FAILED"),
    }
}
