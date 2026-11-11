// The Windows app: a window around the interface in `ui/`, with the core behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::Engine;
use binning_core::app::App;
use binning_core::media::{save_grabbed_frame, FrameGrabber, FrameRequest};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

struct Core(Arc<App>);

/// One frame (or the end, or an error) the window decoded for a clip.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameResult {
    id: u64,
    time_ms: i64,
    total: usize,
    jpeg: Option<String>,
    thumb: Option<String>,
    done: bool,
    error: Option<String>,
}

/// Reads video frames through the window when FFmpeg is not installed: the window plays the
/// clip, draws each frame padded onto a square and sends it back as a JPEG.
struct WindowGrabber {
    handle: AppHandle,
    waiting: Arc<Mutex<HashMap<u64, Sender<FrameResult>>>>,
    next_id: AtomicU64,
}

impl FrameGrabber for WindowGrabber {
    fn grab(&self, request: &FrameRequest, on_frame: &dyn Fn(usize, usize)) -> Result<Vec<(String, i64)>, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.waiting.lock().insert(id, tx);
        let asked = self.handle.emit(
            "binning",
            json!({"name": "grab", "payload": {"id": id, "path": request.source, "durationMs": request.duration_ms, "maxFrames": request.max_frames, "edge": request.edge}}),
        );
        let mut frames = Vec::new();
        let result = (|| {
            asked.map_err(|e| e.to_string())?;
            loop {
                let r = rx.recv_timeout(Duration::from_secs(45)).map_err(|_| "The window did not answer in time".to_string())?;
                if let Some(e) = r.error {
                    return Err(e);
                }
                if r.done {
                    return Ok(());
                }
                let decode = |b64: &str| base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string());
                if let Some(thumb) = r.thumb.as_deref() {
                    std::fs::write(request.thumb_path, decode(thumb)?).map_err(|e| e.to_string())?;
                }
                if let Some(jpeg) = r.jpeg.as_deref() {
                    let file = save_grabbed_frame(request.out_dir, r.time_ms, &decode(jpeg)?).map_err(|e| e.to_string())?;
                    frames.push((file, r.time_ms));
                    on_frame(frames.len(), r.total);
                }
            }
        })();
        self.waiting.lock().remove(&id);
        result.map(|()| frames)
    }
}

struct Grabs(Arc<Mutex<HashMap<u64, Sender<FrameResult>>>>);
struct Awake(Arc<AtomicBool>);

/// Every core command goes through here, off the window's thread.
#[tauri::command]
async fn call(core: State<'_, Core>, cmd: String, args: Value) -> Result<Value, String> {
    let app = core.0.clone();
    tauri::async_runtime::spawn_blocking(move || app.dispatch(&cmd, &args).map_err(|e| e.to_string())).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn frame_result(grabs: State<'_, Grabs>, result: FrameResult) {
    let done = result.done || result.error.is_some();
    let id = result.id;
    let mut waiting = grabs.0.lock();
    if let Some(tx) = waiting.get(&id) {
        let _ = tx.send(result);
    }
    if done {
        waiting.remove(&id);
    }
}

/// Shows a file selected in File Explorer.
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).exists() {
        return Err("That file isn't there any more".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer wants `/select,"<path>"` as one raw argument, with backslashes.
        std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.replace('/', "\\"))).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let dir = std::path::Path::new(&path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        std::process::Command::new("xdg-open").arg(dir).spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Opens a web page in the default browser. Only https links.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://") || url.contains(['"', ' ', '&', '|', '^', '%']) {
        return Err("Only plain https links can be opened".into());
    }
    #[cfg(windows)]
    std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", &url]).spawn().map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    std::process::Command::new("xdg-open").arg(&url).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Keeps Windows from sleeping while a run is in progress.
#[tauri::command]
fn keep_awake(awake: State<'_, Awake>, on: bool) {
    awake.0.store(on, Ordering::SeqCst);
}

fn awake_thread(flag: Arc<AtomicBool>) {
    std::thread::spawn(move || loop {
        if flag.load(Ordering::SeqCst) {
            // Resets the system's idle timer; the display may still turn off.
            #[cfg(windows)]
            unsafe {
                windows_sys::Win32::System::Power::SetThreadExecutionState(windows_sys::Win32::System::Power::ES_SYSTEM_REQUIRED);
            }
        }
        std::thread::sleep(Duration::from_secs(30));
    });
}

fn main() {
    let grabs: Arc<Mutex<HashMap<u64, Sender<FrameResult>>>> = Arc::new(Mutex::new(HashMap::new()));
    let awake = Arc::new(AtomicBool::new(false));
    awake_thread(awake.clone());
    let setup_grabs = grabs.clone();
    tauri::Builder::default()
        // A second launch brings the first window forward: two copies must not share a project.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(Grabs(grabs))
        .manage(Awake(awake))
        .setup(move |app| {
            let handle = app.handle().clone();
            let (data_dir, projects_root) = App::default_dirs();
            let emit_handle = handle.clone();
            let core = App::new(
                data_dir,
                projects_root,
                Arc::new(move |name, payload| {
                    let _ = emit_handle.emit("binning", json!({"name": name, "payload": payload}));
                }),
            );
            core.set_frame_grabber(Arc::new(WindowGrabber { handle, waiting: setup_grabs.clone(), next_id: AtomicU64::new(1) }));
            core.restore_current();
            app.manage(Core(core));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call, frame_result, reveal, open_url, keep_awake])
        .build(tauri::generate_context!())
        .expect("Agentic Binning could not start")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(core) = app.try_state::<Core>() {
                    core.0.shutdown();
                }
            }
        });
}
