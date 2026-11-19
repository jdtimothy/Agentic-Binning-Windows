//! Serves the app's window to a web browser: the same HTML, the same commands, the real core.
//! It exists so the interface can be developed and tested on a computer without Windows or a
//! Snapdragon NPU. It listens on 127.0.0.1 only and has no access control: don't expose it.
//!
//! `cargo run -p binning-devserver -- [--port 4173] [--ui ui] [--data DIR] [--projects DIR] [--simulated]`

use binning_core::app::App;
use parking_lot::{Condvar, Mutex};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, StatusCode};

#[derive(Default)]
struct Events {
    log: Mutex<(u64, Vec<(u64, String, Value)>)>,
    changed: Condvar,
}

impl Events {
    fn push(&self, name: &str, payload: Value) {
        let mut g = self.log.lock();
        g.0 += 1;
        let seq = g.0;
        g.1.push((seq, name.to_string(), payload));
        if g.1.len() > 2000 {
            g.1.drain(..1000);
        }
        self.changed.notify_all();
    }

    /// Events after `since`, waiting up to 20 s for the first one.
    fn after(&self, since: u64) -> Value {
        let mut g = self.log.lock();
        // A `since` from the future is a client asking where the log stands: answer at once.
        if since <= g.0 && g.1.last().is_none_or(|e| e.0 <= since) {
            self.changed.wait_for(&mut g, Duration::from_secs(20));
        }
        let events: Vec<Value> = g.1.iter().filter(|e| e.0 > since).map(|(seq, name, payload)| json!({"seq": seq, "name": name, "payload": payload})).collect();
        json!({"last": g.0, "events": events})
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("header")
}

fn respond_json(request: Request, status: u16, body: &Value) {
    let _ = request.respond(Response::from_string(body.to_string()).with_status_code(StatusCode(status)).with_header(header("Content-Type", "application/json")));
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => match bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(b) => {
                    out.push(b);
                    i += 3;
                }
                None => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Serves a photo, video or thumbnail from disk, with byte ranges so videos can seek.
fn serve_media(request: Request, path: &str) {
    let kind = content_type(path);
    if !(kind.starts_with("image/") || kind.starts_with("video/")) {
        return respond_json(request, 403, &json!({"error": "Only photos and videos are served"}));
    }
    let Ok(mut file) = std::fs::File::open(path) else { return respond_json(request, 404, &json!({"error": "Not found"})) };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let range = request.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str().to_string());
    if let Some(r) = range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        let (a, b) = r.split_once('-').unwrap_or((r, ""));
        let start: u64 = a.parse().unwrap_or(0);
        let end: u64 = b.parse().ok().filter(|e| *e < len).unwrap_or(len.saturating_sub(1)).min(start + 4 * 1024 * 1024);
        if start < len && file.seek(SeekFrom::Start(start)).is_ok() {
            let mut buf = vec![0u8; (end - start + 1) as usize];
            if file.read_exact(&mut buf).is_ok() {
                let _ = request.respond(
                    Response::from_data(buf)
                        .with_status_code(StatusCode(206))
                        .with_header(header("Content-Type", kind))
                        .with_header(header("Accept-Ranges", "bytes"))
                        .with_header(header("Content-Range", &format!("bytes {start}-{end}/{len}"))),
                );
                return;
            }
        }
    }
    let _ = request.respond(Response::from_file(file).with_header(header("Content-Type", kind)).with_header(header("Accept-Ranges", "bytes")).with_header(header("Cache-Control", "no-cache")));
}

fn handle(mut request: Request, app: &Arc<App>, events: &Arc<Events>, ui_dir: &PathBuf) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    if request.method() == &Method::Post {
        if let Some(cmd) = path.strip_prefix("/api/") {
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let args: Value = serde_json::from_str(&body).unwrap_or(json!({}));
            return match app.dispatch(cmd, &args) {
                Ok(value) => respond_json(request, 200, &json!({"ok": true, "value": value})),
                Err(e) => respond_json(request, 200, &json!({"ok": false, "error": e.to_string()})),
            };
        }
        return respond_json(request, 404, &json!({"error": "Not found"}));
    }
    if path == "/events" {
        let since = query_param(&url, "since").and_then(|s| s.parse().ok()).unwrap_or(0);
        return respond_json(request, 200, &events.after(since));
    }
    if path == "/file" {
        return match query_param(&url, "path") {
            Some(p) => serve_media(request, &p),
            None => respond_json(request, 400, &json!({"error": "No path"})),
        };
    }
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    if rel.contains("..") {
        return respond_json(request, 403, &json!({"error": "No"}));
    }
    match std::fs::File::open(ui_dir.join(rel)) {
        Ok(file) => {
            let _ = request.respond(Response::from_file(file).with_header(header("Content-Type", content_type(rel))).with_header(header("Cache-Control", "no-store")));
        }
        Err(_) => respond_json(request, 404, &json!({"error": "Not found"})),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let port = flag("--port").unwrap_or_else(|| "4173".into());
    let ui_dir = PathBuf::from(flag("--ui").unwrap_or_else(|| "ui".into()));
    let (default_data, default_projects) = App::default_dirs();
    let data_dir = flag("--data").map(PathBuf::from).unwrap_or(default_data);
    let projects_root = flag("--projects").unwrap_or(default_projects);
    let events = Arc::new(Events::default());
    let sink = events.clone();
    let app = App::new(data_dir, binning_core::paths::norm(projects_root), Arc::new(move |name, payload| sink.push(name, payload)));
    // Off Windows there is no GenieX: `--simulated` stands the simulated models in for it.
    app.use_simulated_models(args.iter().any(|a| a == "--simulated"));
    app.restore_current();
    let server = tiny_http::Server::http(format!("127.0.0.1:{port}")).expect("could not listen");
    println!("Agentic Binning (browser mode) on http://127.0.0.1:{port}");
    for request in server.incoming_requests() {
        let (app, events, ui_dir) = (app.clone(), events.clone(), ui_dir.clone());
        std::thread::spawn(move || handle(request, &app, &events, &ui_dir));
    }
}
