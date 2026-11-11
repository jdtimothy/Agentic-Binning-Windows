//! GenieX kept running between items. The app starts one `geniex serve` command, as a child
//! process on a port of this computer only, and sends each photo, clip and sorting call to it.
//! The model then stays loaded for the whole run, as it does in the Android app
//! (`PipelineController.runBatch` loads once and releases before sorting).
//!
//! One `geniex infer` per item cost about 16 s a photo on a Snapdragon X2 Elite Extreme, of which
//! about 3 s was the work: the rest was loading the model and the process ending. Through the
//! server the same photo takes about the 3 s (`docs/notes/windows-first-run.md`).
//!
//! Observed on GenieX CLI v0.10.0, and read in `cli/server` of the CLI's source:
//!
//! - `geniex serve --host 127.0.0.1:<port>` answers `GET /health` as soon as it listens. No model
//!   is loaded until a request names one.
//! - `POST /v1/chat/completions` is the OpenAI shape plus GenieX's own fields (`nctx`, `compute`,
//!   `enable_think`, `grammar_string`, `top_k`, `min_p`, `repetition_penalty`). An image is an
//!   `image_url` part whose URL is a file path; paths with spaces work.
//! - The server holds one model. A request for another model unloads the first, so going from
//!   analysis to sorting swaps models by itself (about 12 s). A request with no messages only
//!   loads the model.
//! - With `stream` the reply arrives as `data:{...}` lines: one per token, then a line with
//!   `finish_reason` (`stop` or `length`), then one with `usage` and `timings`, then `data:[DONE]`.
//! - A request the server refuses is a 400 or 500 with `{"error": "..."}`.
//! - `--keepalive` unloads the model after that many idle seconds; the next request loads it again.
//!
//! When `geniex serve` cannot start (an older GenieX), the caller goes back to `geniex infer`.

use crate::db::Profile;
use crate::geniex::{base_command, InferOutput, InferRequest};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Seconds the server keeps an unused model loaded.
const KEEPALIVE_SECONDS: u32 = 120;
/// How long the server may take to answer `/health` after it starts.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// A request with no byte from the server for this long has failed. Loading a model is the
/// longest silence, and takes seconds.
const READ_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq)]
pub enum ServeError {
    /// `geniex serve` could not be started; `geniex infer` may still work.
    Unavailable(String),
    /// The server ran and the request failed.
    Failed(String),
}

struct Running {
    child: Child,
    port: u16,
    program: String,
    compute: String,
}

/// One `geniex serve` child process, started when first needed and stopped with the app.
pub struct GenieServer {
    scratch_dir: String,
    running: Mutex<Option<Running>>,
    /// The program whose `serve` did not start, so it is not tried again for every item.
    failed: Mutex<Option<(String, String)>>,
}

/// What a streamed reply came to.
#[derive(Debug, Default, PartialEq)]
struct StreamEnd {
    text: String,
    finish_reason: Option<String>,
    prompt_tokens: i64,
    generated_tokens: i64,
    decoding_speed: f64,
    /// Milliseconds until the first piece of the reply.
    ttft_ms: f64,
}

/// Reads the `data:` lines of a streamed reply. `on_delta` gets each piece of text; reading stops
/// early once `abandon` is set. `started` is when the request was sent.
fn read_stream(reader: impl BufRead, started: Instant, abandon: &AtomicBool, on_delta: &mut dyn FnMut(&str)) -> Result<StreamEnd, String> {
    let mut end = StreamEnd::default();
    let mut first = true;
    for line in reader.lines() {
        let line = line.map_err(|e| format!("The connection to geniex serve broke: {e}"))?;
        if abandon.load(Ordering::SeqCst) {
            break;
        }
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(e) = v.get("error") {
            return Err(error_text(e));
        }
        if let Some(choice) = v["choices"].get(0) {
            if let Some(piece) = choice["delta"]["content"].as_str().filter(|p| !p.is_empty()) {
                if first {
                    end.ttft_ms = started.elapsed().as_secs_f64() * 1000.0;
                    first = false;
                }
                end.text.push_str(piece);
                on_delta(piece);
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                end.finish_reason = Some(reason.to_string());
            }
        }
        if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
            end.prompt_tokens = usage["prompt_tokens"].as_i64().unwrap_or(0);
            end.generated_tokens = usage["completion_tokens"].as_i64().unwrap_or(0);
        }
        if let Some(speed) = v["timings"]["predicted_per_second"].as_f64() {
            end.decoding_speed = speed;
        }
    }
    Ok(end)
}

/// The `error` field of a refused request: a sentence, or an object with a `message`.
fn error_text(e: &Value) -> String {
    let text = e.as_str().or_else(|| e["message"].as_str()).map(str::to_string).unwrap_or_else(|| e.to_string());
    text.chars().take(300).collect()
}

/// The body of one chat request, in the shape `geniex serve` reads.
pub fn chat_body(req: &InferRequest, compute: &str) -> Value {
    let user = if req.images.is_empty() {
        json!(req.user)
    } else {
        let mut parts: Vec<Value> = req.images.iter().map(|p| json!({"type": "image_url", "image_url": {"url": p}})).collect();
        parts.push(json!({"type": "text", "text": req.user}));
        json!(parts)
    };
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    messages.push(json!({"role": "user", "content": user}));
    let mut body = load_body(req.model, req.nctx, compute);
    // The sampler the LFM3 model card gives for general use; min_p just above zero turns it off.
    for (k, v) in [
        ("stream", json!(true)),
        ("stream_options", json!({"include_usage": true})),
        ("max_tokens", json!(req.max_tokens)),
        ("temperature", json!(0.1)),
        ("top_k", json!(50)),
        ("top_p", json!(1.0)),
        ("min_p", json!(0.000001)),
        ("repetition_penalty", json!(1.1)),
        ("messages", json!(messages)),
    ] {
        body[k] = v;
    }
    if let Some(g) = req.grammar {
        body["grammar_string"] = json!(g);
    }
    body
}

/// A request with no messages: the server loads the model and answers. The fields that decide
/// whether the server reloads (`model`, `nctx`, `compute`) are the ones every chat request sends.
fn load_body(model: &str, nctx: u32, compute: &str) -> Value {
    json!({"model": model, "nctx": nctx, "compute": compute, "enable_think": false, "messages": []})
}

enum Event {
    Delta(String),
    Done(Result<StreamEnd, String>),
}

fn post(url: &str, body: &str, abandon: &AtomicBool, on_delta: &mut dyn FnMut(&str)) -> Result<StreamEnd, String> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout_read(READ_TIMEOUT).build();
    let started = Instant::now();
    match agent.post(url).set("Content-Type", "application/json").send_string(body) {
        Ok(response) => read_stream(BufReader::new(response.into_reader()), started, abandon, on_delta),
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            let said = serde_json::from_str::<Value>(&text).ok().and_then(|v| v.get("error").map(error_text)).unwrap_or(text);
            Err(format!("geniex serve refused the request ({code}): {}", said.chars().take(300).collect::<String>()))
        }
        Err(e) => Err(format!("geniex serve did not answer: {e}")),
    }
}

impl GenieServer {
    pub fn new(scratch_dir: String) -> Self {
        Self { scratch_dir, running: Mutex::new(None), failed: Mutex::new(None) }
    }

    fn serve_args(port: u16, compute: &str) -> Vec<String> {
        ["--skip-update", "serve", "--host", &format!("127.0.0.1:{port}"), "--compute", compute, "--keepalive", &KEEPALIVE_SECONDS.to_string()].iter().map(|s| s.to_string()).collect()
    }

    /// The port of the running server, starting it (or starting it again after a change of
    /// program or compute unit, or after it died) when needed.
    fn ensure(&self, program: &str, compute: &str) -> Result<u16, ServeError> {
        let mut running = self.running.lock();
        if let Some(r) = running.as_mut() {
            if r.program == program && r.compute == compute && matches!(r.child.try_wait(), Ok(None)) {
                return Ok(r.port);
            }
            let _ = r.child.kill();
            let _ = r.child.wait();
            *running = None;
        }
        if let Some((p, why)) = self.failed.lock().as_ref() {
            if p == program {
                return Err(ServeError::Unavailable(why.clone()));
            }
        }
        let unavailable = |why: String| {
            *self.failed.lock() = Some((program.to_string(), why.clone()));
            ServeError::Unavailable(why)
        };
        let _ = std::fs::create_dir_all(&self.scratch_dir);
        let log_path = format!("{}/geniex-serve.log", self.scratch_dir);
        let log = std::fs::File::create(&log_path).ok();
        let out = |f: &Option<std::fs::File>| f.as_ref().and_then(|f| f.try_clone().ok()).map(Stdio::from).unwrap_or_else(Stdio::null);
        // Port 0 asks the system for a free port; it is free again once the listener is dropped.
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| unavailable(format!("No free port for geniex serve: {e}")))?;
        let mut child = base_command(program)
            .env("GENIEX_LOG", "error")
            .args(Self::serve_args(port, compute))
            .stdout(out(&log))
            .stderr(out(&log))
            .spawn()
            .map_err(|e| unavailable(format!("Could not start geniex serve: {e}")))?;
        end_with_this_process(&child);
        let started = Instant::now();
        let health = format!("http://127.0.0.1:{port}/health");
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let said = std::fs::read_to_string(&log_path).ok().and_then(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).last().map(|l| l.chars().take(200).collect::<String>())).unwrap_or_default();
                return Err(unavailable(format!("geniex serve ended at once ({status}): {said}")));
            }
            if ureq::get(&health).timeout(Duration::from_secs(1)).call().is_ok() {
                break;
            }
            if started.elapsed() > START_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!("geniex serve did not answer within {} seconds", START_TIMEOUT.as_secs())));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        *running = Some(Running { child, port, program: program.to_string(), compute: compute.to_string() });
        Ok(port)
    }

    /// Sends `body` and passes the reply on as it grows. A call `cancelled` while it waits
    /// returns at once with what had arrived; the request is left to end by itself.
    fn send(&self, port: u16, body: String, cancelled: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<(StreamEnd, bool), ServeError> {
        let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
        let abandon = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Event>();
        let worker_abandon = abandon.clone();
        std::thread::spawn(move || {
            let delta_tx = tx.clone();
            let result = post(&url, &body, &worker_abandon, &mut |piece| {
                let _ = delta_tx.send(Event::Delta(piece.to_string()));
            });
            let _ = tx.send(Event::Done(result));
        });
        let mut text = String::new();
        loop {
            if cancelled.load(Ordering::SeqCst) {
                abandon.store(true, Ordering::SeqCst);
                return Ok((StreamEnd { text, ..Default::default() }, true));
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Delta(piece)) => {
                    text.push_str(&piece);
                    on_text(&text);
                }
                Ok(Event::Done(result)) => return result.map(|end| (end, false)).map_err(ServeError::Failed),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(ServeError::Failed("The request to geniex serve ended without a result".into())),
            }
        }
    }

    /// Starts the server if needed and has it load `model`, so the first item does not wait for it.
    pub fn load(&self, program: &str, compute: &str, model: &str, nctx: u32, cancelled: &AtomicBool) -> Result<(), ServeError> {
        let port = self.ensure(program, compute)?;
        self.send(port, load_body(model, nctx, compute).to_string(), cancelled, &mut |_| {}).map(|_| ())
    }

    /// One reply from the running server. `on_text` receives the whole reply so far, as it
    /// grows. Returns the reply and the commands behind it, for the activity log.
    pub fn chat(&self, program: &str, compute: &str, req: &InferRequest, cancelled: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<(InferOutput, String), ServeError> {
        let port = self.ensure(program, compute)?;
        let body = chat_body(req, compute).to_string();
        let request_file = format!("{}/request.json", self.scratch_dir);
        let _ = std::fs::write(&request_file, &body);
        let command = format!("geniex {}  then: curl http://127.0.0.1:{port}/v1/chat/completions -H \"Content-Type: application/json\" -d \"@{request_file}\"", Self::serve_args(port, compute).join(" "));
        let (end, stopped) = self.send(port, body, cancelled, on_text)?;
        let stop_reason = match (stopped, end.finish_reason.as_deref()) {
            (true, _) => "user",
            (false, Some("length")) => "length",
            (false, Some(_)) => "eos",
            (false, None) => return Err(ServeError::Failed("geniex serve ended the reply without saying why".into())),
        };
        let profile = Profile { stop_reason: stop_reason.into(), ttft_ms: end.ttft_ms, prompt_tokens: end.prompt_tokens, generated_tokens: end.generated_tokens, decoding_speed: end.decoding_speed };
        Ok((InferOutput { text: end.text, profile }, command))
    }

    /// Ends the server, which unloads the model. The next call starts it again, and tries again
    /// a program whose `serve` did not start before.
    pub fn stop(&self) {
        *self.failed.lock() = None;
        if let Some(mut r) = self.running.lock().take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
    }
}

impl Drop for GenieServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Makes Windows end the server when this process ends, however it ends (closed, crashed, or
/// ended from Task Manager), so no `geniex` is left holding the model. The child joins a job
/// object that kills its processes when its last handle closes; the handle is never closed here,
/// so that happens when this process is gone.
#[cfg(windows)]
fn end_with_this_process(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    type Handle = *mut std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io_counters: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const std::ffi::c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const std::ffi::c_void, length: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    }
    const EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const KILL_ON_JOB_CLOSE: u32 = 0x2000;
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| {
        // SAFETY: plain Win32 calls with a zeroed, correctly sized structure.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if !job.is_null() {
                let limits = ExtendedLimits { basic: BasicLimits { limit_flags: KILL_ON_JOB_CLOSE, ..Default::default() }, ..Default::default() };
                SetInformationJobObject(job, EXTENDED_LIMIT_INFORMATION, &limits as *const _ as *const std::ffi::c_void, std::mem::size_of::<ExtendedLimits>() as u32);
            }
            job as usize
        }
    });
    if job != 0 {
        // SAFETY: both handles are valid for the duration of the call.
        unsafe {
            AssignProcessToJobObject(job as Handle, child.as_raw_handle() as Handle);
        }
    }
}

#[cfg(not(windows))]
fn end_with_this_process(_child: &Child) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real streamed reply of `geniex serve` (GenieX CLI v0.10.0, LFM3-VL-2B Q4_0 on the NPU),
    /// with the tokens between the first two and the last left out.
    const REAL_STREAM: &str = concat!(
        "data:{\"id\":\"chatcmpl-d3504d610df7eca9\",\"created\":1794435871,\"model\":\"LiquidAI/LFM3-VL-2B-GGUF:Q4_0\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"The\"},\"finish_reason\":null}]}\n\n",
        "data:{\"id\":\"chatcmpl-d3504d610df7eca9\",\"created\":1794435871,\"model\":\"LiquidAI/LFM3-VL-2B-GGUF:Q4_0\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\" image\"},\"finish_reason\":null}]}\n\n",
        "data:{\"id\":\"chatcmpl-d3504d610df7eca9\",\"created\":1794435871,\"model\":\"LiquidAI/LFM3-VL-2B-GGUF:Q4_0\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\".\"},\"finish_reason\":null}]}\n\n",
        "data:{\"id\":\"chatcmpl-d3504d610df7eca9\",\"created\":1794435871,\"model\":\"LiquidAI/LFM3-VL-2B-GGUF:Q4_0\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data:{\"id\":\"chatcmpl-d3504d610df7eca9\",\"created\":1794435871,\"model\":\"LiquidAI/LFM3-VL-2B-GGUF:Q4_0\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"completion_tokens\":14,\"prompt_tokens\":330,\"total_tokens\":344,\"completion_tokens_details\":{\"accepted_prediction_tokens\":0,\"audio_tokens\":0,\"reasoning_tokens\":0,\"rejected_prediction_tokens\":0,\"text_tokens\":0},\"prompt_tokens_details\":{\"audio_tokens\":0,\"cache_write_tokens\":0,\"cached_tokens\":0,\"image_tokens\":0,\"text_tokens\":0}},\"timings\":{\"prompt_n\":330,\"prompt_ms\":191.44,\"prompt_per_second\":1723.7776849143336,\"predicted_n\":14,\"predicted_ms\":217.72,\"predicted_per_second\":64.30277420540143}}\n\n",
        "data:[DONE]\n\n",
    );

    #[test]
    fn reads_a_real_streamed_reply() {
        let mut pieces = Vec::new();
        let end = read_stream(REAL_STREAM.as_bytes(), Instant::now(), &AtomicBool::new(false), &mut |p| pieces.push(p.to_string())).unwrap();
        assert_eq!(pieces, vec!["The", " image", "."]);
        assert_eq!(end.text, "The image.");
        assert_eq!(end.finish_reason.as_deref(), Some("stop"));
        assert_eq!((end.prompt_tokens, end.generated_tokens), (330, 14));
        assert!((end.decoding_speed - 64.30277420540143).abs() < 1e-9);
    }

    #[test]
    fn a_stream_that_reports_an_error_fails_with_its_words() {
        let stream = "data:{\"choices\":[{\"index\":0,\"delta\":{\"content\":\"{\"},\"finish_reason\":null}]}\n\ndata:{\"error\":\"SDKError(Model failed to load)\",\"code\":12}\n\n";
        let err = read_stream(stream.as_bytes(), Instant::now(), &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert_eq!(err, "SDKError(Model failed to load)");
        // A reply cut short has no finish reason, which the caller reports.
        let cut = read_stream("data:{\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n".as_bytes(), Instant::now(), &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!((cut.text.as_str(), cut.finish_reason), ("a", None));
    }

    #[test]
    fn an_abandoned_stream_stops_being_read() {
        let abandon = AtomicBool::new(true);
        let end = read_stream(REAL_STREAM.as_bytes(), Instant::now(), &abandon, &mut |_| panic!("nothing is passed on after a stop")).unwrap();
        assert_eq!(end.text, "");
    }

    #[test]
    fn builds_the_chat_request() {
        let images = vec!["C:/work/1/f 0.jpg".to_string(), "C:/work/1/f_500.jpg".to_string()];
        let req = InferRequest { model: "LiquidAI/LFM3-VL-2B-GGUF:Q4_0", system: "sys", user: "Video: 2 frames", images: &images, grammar: Some("root ::= \"x\""), max_tokens: 520, nctx: 16384 };
        let body = chat_body(&req, "npu");
        assert_eq!(body["model"], "LiquidAI/LFM3-VL-2B-GGUF:Q4_0");
        assert_eq!((body["stream"].as_bool(), body["enable_think"].as_bool()), (Some(true), Some(false)));
        assert_eq!((body["nctx"].as_u64(), body["max_tokens"].as_u64(), body["compute"].as_str()), (Some(16384), Some(520), Some("npu")));
        assert_eq!((body["temperature"].as_f64(), body["top_k"].as_i64(), body["repetition_penalty"].as_f64()), (Some(0.1), Some(50), Some(1.1)));
        assert!(body["min_p"].as_f64().unwrap() > 0.0, "min_p of 0 would mean the SDK's default");
        assert_eq!(body["grammar_string"], "root ::= \"x\"");
        assert_eq!(body["messages"][0], json!({"role": "system", "content": "sys"}));
        // The frames come first, in order, then the text: the order `geniex infer` gives them.
        let parts = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0]["image_url"]["url"], "C:/work/1/f 0.jpg");
        assert_eq!(parts[2], json!({"type": "text", "text": "Video: 2 frames"}));
        // A sorting call is plain text and may have no grammar.
        let text = InferRequest { model: "m", system: "", user: "hello", images: &[], grammar: None, max_tokens: 10, nctx: 4096 };
        let body = chat_body(&text, "cpu");
        assert_eq!(body["messages"], json!([{"role": "user", "content": "hello"}]));
        assert!(body.get("grammar_string").is_none());
        // The load request and the chat request agree on what makes the server reload.
        let load = load_body("m", 4096, "cpu");
        assert_eq!((&load["model"], &load["nctx"], &load["compute"]), (&body["model"], &body["nctx"], &body["compute"]));
        assert_eq!(load["messages"], json!([]));
    }

    #[test]
    fn a_program_that_cannot_serve_is_reported_once_and_not_tried_again() {
        let d = tempfile::tempdir().unwrap();
        let server = GenieServer::new(d.path().join("scratch").to_string_lossy().to_string());
        let missing = d.path().join("no-such-geniex").to_string_lossy().to_string();
        let first = server.ensure(&missing, "npu").unwrap_err();
        assert!(matches!(&first, ServeError::Unavailable(why) if why.contains("Could not start geniex serve")), "{first:?}");
        assert_eq!(server.ensure(&missing, "npu").unwrap_err(), first);
    }
}
