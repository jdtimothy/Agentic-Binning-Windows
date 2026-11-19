//! GenieX through its command line. The app runs the same `geniex` commands a person would type
//! in a terminal, as child processes, and reads what they print:
//!
//! - `geniex --version` and `geniex list --format json` to see what is installed,
//! - `geniex pull <model>:<precision>` to download a model,
//! - `geniex serve`, started once and kept running, which every photo, video and sorting call is
//!   sent to so the model stays loaded between them (see [`crate::geniex_serve`]),
//! - `geniex infer <model>:<precision> ...` once per call instead, when `geniex serve` cannot start.
//!
//! How `geniex infer` behaves was first read from the CLI's source (`cli/cmd/geniex/infer.go` and
//! `common/process.go`, Nov 2026), then checked by a real run on GenieX CLI v0.10.0 (QAIRT 2.50,
//! LFM3-VL-2B Q4_0 on the NPU). Observed on v0.10.0:
//!
//! - Every flag the app sends is accepted, including `--grammar-path`.
//! - `-i <file>` ends the session by itself; the CLI does not wait for more input.
//! - With `--test-mode`, stderr holds one JSON line with `Input`, `Output`, `Profile` and `Error`.
//! - Stdout starts with status lines (`loading model...`, `Model loaded: ...`, `encoding...`, two
//!   of them behind an emoji), then the echoed prompt (`> ` and `. ` lines), then the reply, then
//!   the verbose block, which ends with `stop reason:    eos`.
//!
//! Still read from source only: the failure messages, and how a call that never loads the model
//! ends. The tests use the real output (shortened image path) where they have it.
//!
//! The points this module depends on:
//!
//! - `-i <file>` reads the prompt from a file, so no prompt text has to survive Windows command
//!   line quoting. `-s` sets the system prompt, `--grammar-path` a GBNF grammar file.
//! - For a vision model the CLI finds image paths inside the prompt text (anything that looks
//!   like an absolute path ending in .jpg/.jpeg/.png/.webp), checks each exists, removes them
//!   from the text and attaches the images after it. So each frame goes on its own line as an
//!   absolute path with forward slashes, and no other line of the prompt may contain a slash.
//! - `--verbose` prints the reply's profile (`stop reason:`, `decode tokens:` ...) after it, and
//!   the hidden `--test-mode` flag writes one JSON line to stderr with the exact reply text and
//!   profile. The JSON is used when present; the printed profile is the fallback.
//! - With `NO_COLOR=1` there is no spinner and no colour codes.
//! - Sampler flags left at 0 mean "the SDK's default", so min-p is sent as 0.000001 to turn it off.

use crate::analysis::{budget, prompt};
use crate::db::Profile;
use crate::geniex_serve::{GenieServer, ServeError};
use crate::media::{find_program, quiet_command, PreparedMedia};
use crate::models::{ReplyFormat, SortUnavailable, TextError, TextModel, TextReply, VisionError, VisionModel, VisionReply};
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// A model GenieX can pull: `repo` is the hub identifier, `precision` picks the weights file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: &'static str,
    pub display_name: &'static str,
    pub repo: &'static str,
    pub precision: &'static str,
    /// `vlm` or `llm`, passed to `geniex pull --model-type`.
    pub model_type: &'static str,
    /// The download size as people read it.
    pub download_size: &'static str,
}

impl ModelSpec {
    /// `<repo>:<precision>`, the name `geniex infer` and `geniex pull` take.
    pub fn key(&self) -> String {
        format!("{}:{}", self.repo, self.precision)
    }
}

pub const LFM3_VL_2B: ModelSpec = ModelSpec {
    id: "lfm3-vl-2b",
    display_name: "LFM3-VL-2B",
    repo: "LiquidAI/LFM3-VL-2B-GGUF",
    precision: "Q4_0",
    model_type: "vlm",
    download_size: "1.4 GB",
};

pub const LFM3_1_7B: ModelSpec =
    ModelSpec { id: "lfm3-1.7b", display_name: "LFM3-1.7B", repo: "LiquidAI/LFM3-1.7B-GGUF", precision: "Q4_0", model_type: "llm", download_size: "1.05 GB" };

/// Context for the sorting model: its budget assumes 16K.
pub const TEXT_CONTEXT: u32 = 16384;

/// Where the `geniex` program is, or None when it is not installed.
pub fn locate(configured: Option<&str>) -> Option<String> {
    let mut extra: Vec<PathBuf> = Vec::new();
    for var in ["LOCALAPPDATA", "ProgramFiles", "USERPROFILE"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            // "GenieX CLI" is where its installer (and `winget install Qualcomm.GenieX.CLI`) puts it.
            for sub in ["GenieX CLI", "geniex", "GenieX", "Programs/geniex", "Programs/GenieX", ".geniex/bin", "geniex/bin", "GenieX/bin"] {
                extra.push(base.join(sub));
            }
        }
    }
    find_program("geniex", configured, &extra)
}

fn strip_ansi(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap()).replace_all(s, "").to_string()
}

/// The CLI with the environment every call shares.
pub(crate) fn base_command(program: &str) -> Command {
    let mut cmd = quiet_command(program);
    cmd.env("NO_COLOR", "1").env("GENIEX_LOG", "none");
    cmd
}

/// One cached model as `geniex list --format json` reports it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ListedModel {
    pub name: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub runtime: String,
    #[serde(default, rename = "type")]
    pub model_type: String,
    #[serde(default)]
    pub precisions: Vec<String>,
}

/// Reads the JSON `geniex list --format json` printed, tolerating text around it.
pub fn parse_list(stdout: &str) -> Vec<ListedModel> {
    let text = strip_ansi(stdout);
    let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) else { return Vec::new() };
    if end < start {
        return Vec::new();
    }
    serde_json::from_str(&text[start..=end]).unwrap_or_default()
}

pub fn has_model(models: &[ListedModel], spec: &ModelSpec) -> bool {
    models.iter().any(|m| {
        m.name.eq_ignore_ascii_case(spec.repo) && (m.precisions.is_empty() || m.precisions.iter().any(|p| p.eq_ignore_ascii_case(spec.precision) || p == "N/A"))
    })
}

/// What the app knows about the GenieX installation.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GenieStatus {
    /// The program's path, or None when it was not found.
    pub path: Option<String>,
    pub version: Option<String>,
    pub models: Vec<ListedModel>,
    pub vision_ready: bool,
    pub text_ready: bool,
    /// Why the list could not be read, when it could not.
    pub error: Option<String>,
}

pub fn status(configured: Option<&str>) -> GenieStatus {
    let Some(path) = locate(configured) else { return GenieStatus::default() };
    let version = base_command(&path)
        .args(["--skip-update", "--version"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| strip_ansi(&String::from_utf8_lossy(&o.stdout)).lines().next().unwrap_or("").trim().to_string())
        .filter(|v| !v.is_empty());
    let (models, error) = match base_command(&path).args(["--skip-update", "list", "--format", "json"]).output() {
        Ok(o) if o.status.success() => (parse_list(&String::from_utf8_lossy(&o.stdout)), None),
        Ok(o) => (Vec::new(), Some(last_line(&String::from_utf8_lossy(&o.stderr)).unwrap_or_else(|| "geniex list failed".into()))),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    GenieStatus { vision_ready: has_model(&models, &LFM3_VL_2B), text_ready: has_model(&models, &LFM3_1_7B), path: Some(path), version, models, error }
}

fn last_line(text: &str) -> Option<String> {
    strip_ansi(text).lines().map(str::trim).filter(|l| !l.is_empty()).last().map(|l| l.chars().take(300).collect())
}

/// Runs `geniex pull` for `spec`, reporting the percentage the progress bar prints. Returns an
/// error sentence when the download did not finish. `cancel` stops it; a later pull resumes.
pub fn pull(program: &str, spec: &ModelSpec, cancel: &AtomicBool, on_progress: &dyn Fn(f64)) -> Result<(), String> {
    let mut child = base_command(program)
        .args(["--skip-update", "pull", &spec.key(), "--model-type", spec.model_type, "--model-hub", "hf"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start geniex: {e}"))?;
    let mut stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let percent = Regex::new(r"(\d{1,3}(?:\.\d+)?)\s*%").unwrap();
    let mut tail = String::new();
    if let Some(mut out) = child.stdout.take() {
        let mut buf = [0u8; 4096];
        loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                break;
            }
            match out.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some(m) = percent.captures_iter(&tail).last() {
                        if let Ok(p) = m[1].parse::<f64>() {
                            on_progress(p.clamp(0.0, 100.0));
                        }
                    }
                    if tail.len() > 2000 {
                        let mut cut = tail.len() - 500;
                        while !tail.is_char_boundary(cut) {
                            cut += 1;
                        }
                        tail = tail[cut..].to_string();
                    }
                }
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let stderr_text = err_thread.join().unwrap_or_default();
    if cancel.load(Ordering::SeqCst) {
        return Err("Download cancelled".into());
    }
    if status.success() && !strip_ansi(&tail).contains("Download cancelled") {
        on_progress(100.0);
        return Ok(());
    }
    Err(last_line(&stderr_text).or_else(|| last_line(&tail)).unwrap_or_else(|| "The download did not finish".into()))
}

/// The profile block `--test-mode` writes to stderr (times in microseconds).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct GoProfile {
    #[serde(default, rename = "TTFT")]
    ttft: i64,
    #[serde(default)]
    prompt_tokens: i64,
    #[serde(default)]
    generated_tokens: i64,
    #[serde(default)]
    decoding_speed: f64,
    #[serde(default)]
    stop_reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TestModeLine {
    #[serde(default)]
    output: String,
    #[serde(default)]
    profile: GoProfile,
}

/// What one `geniex infer` call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct InferOutput {
    pub text: String,
    pub profile: Profile,
}

/// Finds the `--test-mode` JSON line in stderr.
fn parse_test_mode(stderr: &str) -> Option<InferOutput> {
    for line in stderr.lines().rev() {
        let line = line.trim();
        if !line.starts_with('{') || !line.contains("\"Profile\"") {
            continue;
        }
        if let Ok(t) = serde_json::from_str::<TestModeLine>(line) {
            return Some(InferOutput {
                text: t.output,
                profile: Profile {
                    stop_reason: t.profile.stop_reason,
                    ttft_ms: t.profile.ttft as f64 / 1000.0,
                    prompt_tokens: t.profile.prompt_tokens,
                    generated_tokens: t.profile.generated_tokens,
                    decoding_speed: t.profile.decoding_speed,
                },
            });
        }
    }
    None
}

/// A line the CLI prints before the reply: blank, the echoed prompt (`> ` and `. `), or a status
/// line (`loading model...`, `Model loaded: ...`, `encoding...`, the first and last behind an emoji).
fn is_preamble(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || line.starts_with("> ") || line.starts_with(". ") || t.starts_with("Model loaded:") || t.ends_with("loading model...") || t.ends_with("encoding...")
}

/// Reads the reply and the `--verbose` profile from what the CLI printed. The status lines and
/// the echoed prompt come first; the reply follows; the profile block ends it.
pub fn parse_stdout(stdout: &str) -> InferOutput {
    let text = strip_ansi(stdout).replace('\r', "");
    let field = |label: &str| -> Option<String> {
        text.lines().rev().find_map(|l| l.trim().strip_prefix(label).map(|v| v.trim().to_string()))
    };
    let number = |label: &str| -> Option<f64> { field(label).and_then(|v| v.split(|c: char| !(c.is_ascii_digit() || c == '.')).next().and_then(|n| n.parse().ok())) };
    let profile = Profile {
        stop_reason: field("stop reason:").unwrap_or_default(),
        ttft_ms: number("ttft:").map(|s| s * 1000.0).unwrap_or(0.0),
        prompt_tokens: number("prompt tokens:").unwrap_or(0.0) as i64,
        generated_tokens: number("decode tokens:").unwrap_or(0.0) as i64,
        decoding_speed: number("decode speed:").unwrap_or(0.0),
    };
    let body_end = text.rfind("\ntotal time:").unwrap_or(text.len());
    let body: Vec<&str> = text[..body_end].lines().skip_while(|l| is_preamble(l)).collect();
    InferOutput { text: body.join("\n").trim().to_string(), profile }
}

/// A sentence for a failed call, from what the CLI wrote.
fn failure_text(stderr: &str, stdout: &str) -> String {
    let err: Vec<String> = strip_ansi(stderr).lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('{')).map(str::to_string).collect();
    let picked = err.iter().find(|l| l.contains("Error") || l.contains("failed") || l.contains("Oops")).or(err.first()).cloned();
    picked.or_else(|| last_line(stdout)).unwrap_or_else(|| "geniex ended without a reply".into()).chars().take(300).collect()
}

/// The sampler the LFM3 model card gives for general use.
const SAMPLER_ARGS: [&str; 10] = ["--temperature", "0.1", "--top-k", "50", "--top-p", "1.0", "--min-p", "0.000001", "--repetition-penalty", "1.1"];

/// Runs GenieX commands one at a time and lets another thread stop the one that is running.
pub struct GenieCli {
    pub program: String,
    /// `npu`, `gpu` or `cpu`.
    pub compute: String,
    /// Where prompt and grammar files for a call are written.
    pub scratch_dir: String,
    child: Mutex<Option<Child>>,
    cancelled: AtomicBool,
    /// The last command line, shown in Settings so it can be tried by hand in a terminal.
    pub last_command: Mutex<String>,
    /// The running `geniex serve` calls go to. Without one, each call is its own `geniex infer`.
    server: Option<Arc<GenieServer>>,
}

pub struct InferRequest<'a> {
    pub model: &'a str,
    pub system: &'a str,
    /// The text of the user turn, without the image paths.
    pub user: &'a str,
    /// Absolute image paths with forward slashes, in the order the model should see them.
    pub images: &'a [String],
    pub grammar: Option<&'a str>,
    pub max_tokens: u32,
    pub nctx: u32,
}

impl GenieCli {
    pub fn new(program: String, compute: String, scratch_dir: String) -> Self {
        Self { program, compute, scratch_dir, child: Mutex::new(None), cancelled: AtomicBool::new(false), last_command: Mutex::new(String::new()), server: None }
    }

    /// Sends calls to `server`, which keeps the model loaded between them.
    pub fn with_server(mut self, server: Arc<GenieServer>) -> Self {
        self.server = Some(server);
        self
    }

    /// Has the server load `model` now, so the first call does not wait for it. An error means
    /// the model cannot run. Does nothing without a server, or when `geniex serve` cannot start:
    /// `geniex infer` then loads the model in each call.
    pub fn load_model(&self, model: &str, nctx: u32) -> Result<(), String> {
        let Some(server) = &self.server else { return Ok(()) };
        match server.load(&self.program, &self.compute, model, nctx, &self.cancelled) {
            Ok(()) | Err(ServeError::Unavailable(_)) => Ok(()),
            Err(ServeError::Failed(e)) => Err(e),
        }
    }

    /// Stops the running command, if any. Callable from any thread.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(child) = self.child.lock().as_mut() {
            let _ = child.kill();
        }
    }

    pub fn reset_cancel(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }

    /// The arguments of one `geniex infer` call; the prompt and grammar are read from files.
    pub fn infer_args(&self, req: &InferRequest, prompt_file: &str, grammar_file: Option<&str>, system_inline: bool) -> Vec<String> {
        let mut args: Vec<String> = ["--skip-update", "--verbose", "--test-mode", "infer", req.model, "--compute", &self.compute].iter().map(|s| s.to_string()).collect();
        args.extend(["--nctx".to_string(), req.nctx.to_string(), "--max-tokens".to_string(), req.max_tokens.to_string(), "--think=false".to_string()]);
        args.extend(SAMPLER_ARGS.iter().map(|s| s.to_string()));
        if let Some(g) = grammar_file {
            args.extend(["--grammar-path".to_string(), g.to_string()]);
        }
        if system_inline && !req.system.is_empty() {
            args.extend(["-s".to_string(), req.system.to_string()]);
        }
        args.extend(["-i".to_string(), prompt_file.to_string()]);
        args
    }

    /// One call to the model: through the running `geniex serve`, or as one `geniex infer` when
    /// there is none. `on_text` receives everything printed so far, as it grows (from the server
    /// that is the reply alone; `geniex infer` prints the prompt before it). A cancelled call
    /// returns with stop reason `user`.
    pub fn infer(&self, req: &InferRequest, on_text: &mut dyn FnMut(&str)) -> Result<InferOutput, String> {
        std::fs::create_dir_all(&self.scratch_dir).map_err(|e| format!("Cannot create {}: {e}", self.scratch_dir))?;
        let mut no_server = String::new();
        if let Some(server) = &self.server {
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(InferOutput { text: String::new(), profile: Profile { stop_reason: "user".into(), ..Default::default() } });
            }
            match server.chat(&self.program, &self.compute, req, &self.cancelled, on_text) {
                Ok((out, command)) => {
                    *self.last_command.lock() = command;
                    return Ok(out);
                }
                Err(ServeError::Failed(e)) => return Err(e),
                Err(ServeError::Unavailable(why)) => no_server = format!("  (one command per item, because: {why})"),
            }
        }
        let prompt_file = format!("{}/prompt.txt", self.scratch_dir);
        // A .cmd or .bat launcher can't take an argument with quotes and newlines, so the system
        // prompt then goes at the top of the prompt file instead of in `-s`.
        let lower = self.program.to_lowercase();
        let system_inline = !(lower.ends_with(".cmd") || lower.ends_with(".bat"));
        // The CLI is told which images to attach by their paths, each on its own line after the text.
        let user = req.images.iter().fold(req.user.to_string(), |text, path| text + "\n" + path);
        let prompt_text = if system_inline || req.system.is_empty() { user } else { format!("{}\n\n{}", req.system, user) };
        std::fs::write(&prompt_file, prompt_text).map_err(|e| format!("Cannot write {prompt_file}: {e}"))?;
        let grammar_file = match req.grammar {
            Some(g) => {
                let f = format!("{}/grammar.gbnf", self.scratch_dir);
                std::fs::write(&f, g).map_err(|e| format!("Cannot write {f}: {e}"))?;
                Some(f)
            }
            None => None,
        };
        let args = self.infer_args(req, &prompt_file, grammar_file.as_deref(), system_inline);
        *self.last_command.lock() = format!(
            "geniex {}{no_server}",
            args.iter().map(|a| if a.contains([' ', '\n', '"']) { format!("\"{}\"", a.replace('\n', " ").replace('"', "\\\"")) } else { a.clone() }).collect::<Vec<_>>().join(" ")
        );
        if self.cancelled.load(Ordering::SeqCst) {
            return Ok(InferOutput { text: String::new(), profile: Profile { stop_reason: "user".into(), ..Default::default() } });
        }
        let mut child = base_command(&self.program).args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Could not start geniex: {e}"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *self.child.lock() = Some(child);
        let err_thread = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(e) = stderr {
                for line in BufReader::new(e).lines().map_while(Result::ok) {
                    s.push_str(&line);
                    s.push('\n');
                }
            }
            s
        });
        let mut printed = String::new();
        if let Some(mut out) = stdout {
            let mut buf = [0u8; 2048];
            let mut pending: Vec<u8> = Vec::new();
            loop {
                match out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        // Only whole UTF-8 characters are passed on.
                        let valid = match std::str::from_utf8(&pending) {
                            Ok(_) => pending.len(),
                            Err(e) => e.valid_up_to(),
                        };
                        if valid > 0 {
                            printed.push_str(&String::from_utf8_lossy(&pending[..valid]));
                            pending.drain(..valid);
                            on_text(&printed);
                        }
                    }
                }
            }
        }
        let status = self.child.lock().take().map(|mut c| c.wait());
        let stderr_text = err_thread.join().unwrap_or_default();
        if self.cancelled.load(Ordering::SeqCst) {
            let partial = parse_stdout(&printed);
            return Ok(InferOutput { text: partial.text, profile: Profile { stop_reason: "user".into(), ..partial.profile } });
        }
        let ok = matches!(status, Some(Ok(s)) if s.success());
        let parsed = parse_test_mode(&stderr_text).unwrap_or_else(|| parse_stdout(&printed));
        if parsed.profile.stop_reason.is_empty() {
            // No profile at all: the model never ran (not installed, failed to load, bad flag).
            return Err(failure_text(&stderr_text, &printed));
        }
        if !ok && parsed.text.trim().is_empty() {
            return Err(failure_text(&stderr_text, &printed));
        }
        Ok(parsed)
    }
}

/// The user turn for the vision model: the instruction, then each frame's absolute path on its
/// own line (forward slashes), which is how the CLI is told which images to attach.
pub fn vision_prompt(media: &PreparedMedia) -> Result<String, String> {
    let (text, images) = vision_parts(media)?;
    Ok(images.iter().fold(text, |text, path| text + "\n" + path))
}

/// The instruction and the frames' paths (forward slashes), checked for what `geniex infer`
/// could not take: a slash in the text, or a path that is not one line ending in .jpg.
fn vision_parts(media: &PreparedMedia) -> Result<(String, Vec<String>), String> {
    let text = prompt::user_text(media.kind, &media.frame_times_ms);
    if text.contains('/') || text.contains('\\') {
        return Err("The prompt text may not contain a slash".into());
    }
    Ok((text, image_lines(&media.image_paths)?))
}

/// Frame paths with forward slashes, each checked to be one line ending in .jpg.
fn image_lines(paths: &[String]) -> Result<Vec<String>, String> {
    let mut images = Vec::new();
    for p in paths {
        let p = p.replace('\\', "/");
        if p.contains('\n') || !p.to_lowercase().ends_with(".jpg") {
            return Err(format!("Not a usable image path: {p}"));
        }
        images.push(p);
    }
    Ok(images)
}

/// LFM3-VL through GenieX: one fresh single-turn call per photo or clip.
pub struct GenieVisionModel {
    pub cli: Arc<GenieCli>,
    pub spec: ModelSpec,
    pub use_grammar: bool,
    nctx: Mutex<u32>,
}

impl GenieVisionModel {
    pub fn new(cli: Arc<GenieCli>, spec: ModelSpec) -> Self {
        Self { cli, spec, use_grammar: true, nctx: Mutex::new(budget::MIN_CONTEXT) }
    }
}

impl VisionModel for GenieVisionModel {
    fn model_id(&self) -> String {
        self.spec.key()
    }
    fn image_edge(&self) -> u32 {
        budget::FALLBACK_IMAGE_EDGE
    }
    fn load(&self, max_frames: u32) -> Result<(), VisionError> {
        self.cli.reset_cancel();
        let nctx = budget::context_for(max_frames, budget::FALLBACK_TOKENS_PER_FRAME);
        *self.nctx.lock() = nctx;
        self.cli.load_model(&self.spec.key(), nctx).map_err(VisionError)
    }
    fn generate(&self, media: &PreparedMedia, on_token: &mut dyn FnMut(&str)) -> Result<VisionReply, VisionError> {
        let (user, images) = vision_parts(media).map_err(VisionError)?;
        let grammar = self.use_grammar.then(|| prompt::grammar(media.kind));
        let key = self.spec.key();
        let req = InferRequest { model: &key, system: prompt::SYSTEM, user: &user, images: &images, grammar: grammar.as_deref(), max_tokens: prompt::max_tokens(media.kind), nctx: *self.nctx.lock() };
        // The reply is one JSON object; what was printed before its first brace is the echoed prompt.
        let mut sent = 0usize;
        let mut forward = |printed: &str| {
            let Some(start) = printed.find("{\"").or_else(|| printed.find("{\n")).or_else(|| printed.find("{ ")) else { return };
            let body = &printed[start..];
            let end = body.find("\n\ntotal time:").unwrap_or(body.len());
            if end > sent {
                on_token(&body[sent..end]);
                sent = end;
            }
        };
        let out = self.cli.infer(&req, &mut forward).map_err(VisionError)?;
        Ok(VisionReply { text: out.text, profile: out.profile })
    }
    fn describe_moment(&self, image: &str) -> Result<VisionReply, VisionError> {
        let images = image_lines(&[image.to_string()]).map_err(VisionError)?;
        let key = self.spec.key();
        let req = InferRequest { model: &key, system: prompt::MOMENT_SYSTEM, user: prompt::MOMENT_TEXT, images: &images, grammar: Some(prompt::MOMENT_GRAMMAR), max_tokens: prompt::MAX_TOKENS_MOMENT, nctx: *self.nctx.lock() };
        let out = self.cli.infer(&req, &mut |_| {}).map_err(VisionError)?;
        Ok(VisionReply { text: out.text, profile: out.profile })
    }
    fn cancel(&self) {
        self.cli.cancel();
    }
    fn last_command(&self) -> Option<String> {
        Some(self.cli.last_command.lock().clone()).filter(|c| !c.is_empty())
    }
}

/// LFM3-1.7B through `geniex infer`, the reply held to the call's grammar.
pub struct GenieTextModel {
    pub cli: Arc<GenieCli>,
    pub spec: ModelSpec,
}

impl TextModel for GenieTextModel {
    fn model_id(&self) -> String {
        self.spec.key()
    }
    fn load(&self) -> Result<(), TextError> {
        self.cli.reset_cancel();
        let st = status(Some(&self.cli.program));
        if st.error.is_none() && !has_model(&st.models, &self.spec) {
            return Err(TextError::Unavailable(SortUnavailable::ModelNotDownloaded, format!("{} is not downloaded", self.spec.display_name)));
        }
        self.cli.load_model(&self.spec.key(), TEXT_CONTEXT).map_err(|e| TextError::Unavailable(SortUnavailable::ModelLoadFailed, e))
    }
    fn complete(&self, system: &str, user: &str, format: &ReplyFormat, max_tokens: u32) -> Result<TextReply, TextError> {
        let started = Instant::now();
        let key = self.spec.key();
        let req = InferRequest { model: &key, system, user, images: &[], grammar: Some(&format.grammar), max_tokens, nctx: TEXT_CONTEXT };
        let out = self.cli.infer(&req, &mut |_| {}).map_err(|e| TextError::Unavailable(SortUnavailable::ModelLoadFailed, e))?;
        Ok(TextReply {
            text: out.text,
            stop_reason: out.profile.stop_reason,
            input_tokens: out.profile.prompt_tokens,
            output_tokens: out.profile.generated_tokens,
            millis: started.elapsed().as_millis() as i64,
        })
    }
    fn cancel(&self) {
        self.cli.cancel();
    }
    fn last_command(&self) -> Option<String> {
        Some(self.cli.last_command.lock().clone()).filter(|c| !c.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::ItemKind;

    /// Real stdout of `geniex --verbose infer` (GenieX CLI v0.10.0, LFM3-VL-2B Q4_0 on the NPU),
    /// with the image path shortened. The globe emoji is what the CLI prints before its status lines.
    const REAL_STDOUT: &str = "\u{1f30d}loading model...\nModel loaded: runtime=llama_cpp compute=npu ngl=-1 nctx=16384\n> Describe this photo.\n. C:/temp/test.jpg\n\u{1f30d}encoding...\nThe image shows a solid red square centered on a light gray background. The square is the only element present.\n\ntotal time:     2.911081s\nttft:           2.568213s\nmedia time:     2.411717s\nprompt time:    0.154753s\nprompt tokens:  284 token(s)\nprompt speed:   1835.182517 tok/s\ndecode time:    0.344611s\ndecode tokens:  22 token(s)\ndecode speed:   63.840098 tok/s\nstop reason:    eos\n\n";

    /// Real stderr of the same call with `--test-mode`.
    const REAL_STDERR: &str = "{\"Input\":\"Describe this photo.\\nC:/temp/test.jpg\",\"Output\":\"The image shows a solid red square centered on a light gray background. The square is the only element present.\",\"Profile\":{\"TTFT\":2568213,\"MediaTime\":2411717,\"PromptTime\":154753,\"DecodeTime\":344611,\"PromptTokens\":284,\"GeneratedTokens\":22,\"PrefillSpeed\":1835.1825166555736,\"DecodingSpeed\":63.84009796553215,\"DraftNTotal\":0,\"DraftNAccepted\":0,\"StopReason\":\"eos\"},\"Error\":null}\n";

    const REAL_REPLY: &str = "The image shows a solid red square centered on a light gray background. The square is the only element present.";

    #[test]
    fn parses_real_verbose_stdout() {
        let out = parse_stdout(REAL_STDOUT);
        assert_eq!(out.text, REAL_REPLY);
        assert_eq!(out.profile.stop_reason, "eos");
        assert_eq!(out.profile.generated_tokens, 22);
        assert_eq!(out.profile.prompt_tokens, 284);
        assert!((out.profile.ttft_ms - 2568.213).abs() < 0.01);
        assert!((out.profile.decoding_speed - 63.840098).abs() < 0.001);
    }

    #[test]
    fn parses_real_test_mode_json() {
        let out = parse_test_mode(REAL_STDERR).unwrap();
        assert_eq!(out.text, REAL_REPLY);
        assert_eq!(out.profile.stop_reason, "eos");
        assert_eq!(out.profile.prompt_tokens, 284);
        assert_eq!(out.profile.generated_tokens, 22);
        assert!((out.profile.ttft_ms - 2568.213).abs() < 0.001);
        assert!((out.profile.decoding_speed - 63.84009796553215).abs() < 1e-9);
    }

    /// A JSON reply in the same layout, written by hand: the real run answered in plain text.
    const VERBOSE_OUT: &str = "> Describe this photo.\n\n{\"description\": \"A red square.\", \"actions\": [], \"tags\": [\"red\", \"square\", \"shape\"], \"suggested_name\": \"Red Square\", \"shot_type\": \"close\"}\n\ntotal time:     2.500000s\nttft:           0.580000s\nmedia time:     0.380000s\nprompt time:    0.200000s\nprompt tokens:  281 token(s)\nprompt speed:   1405.000000 tok/s\ndecode time:    1.900000s\ndecode tokens:  42 token(s)\ndecode speed:   46.100000 tok/s\nstop reason:    eos\n\n";

    #[test]
    fn parses_verbose_stdout() {
        let out = parse_stdout(VERBOSE_OUT);
        assert!(out.text.starts_with("{\"description\"") && out.text.ends_with('}'));
        assert_eq!(out.profile.stop_reason, "eos");
        assert_eq!(out.profile.generated_tokens, 42);
        assert_eq!(out.profile.prompt_tokens, 281);
        assert!((out.profile.ttft_ms - 580.0).abs() < 0.01);
        assert!((out.profile.decoding_speed - 46.1).abs() < 0.01);
        assert!(crate::analysis::parse(&out.text, ItemKind::Photo).is_some());
    }

    #[test]
    fn parses_test_mode_json() {
        let stderr = "some log\n{\"Input\":\"x\",\"Output\":\"{\\\"a\\\":1}\",\"Profile\":{\"TTFT\":53000,\"MediaTime\":0,\"PromptTokens\":10,\"GeneratedTokens\":7,\"DecodingSpeed\":47.5,\"StopReason\":\"length\"},\"Error\":null}\n";
        let out = parse_test_mode(stderr).unwrap();
        assert_eq!(out.text, "{\"a\":1}");
        assert_eq!(out.profile.stop_reason, "length");
        assert_eq!(out.profile.ttft_ms, 53.0);
        assert!(parse_test_mode("nothing here").is_none());
    }

    #[test]
    fn reads_the_model_list() {
        let models = parse_list("Models cached\n[{\"name\":\"LiquidAI/LFM3-VL-2B-GGUF\",\"size\":1500,\"runtime\":\"llama_cpp\",\"type\":\"vlm\",\"precisions\":[\"Q4_0\"]}]\n");
        assert!(has_model(&models, &LFM3_VL_2B));
        assert!(!has_model(&models, &LFM3_1_7B));
        assert!(parse_list("no json").is_empty());
    }

    #[test]
    fn vision_prompt_lists_frames_on_their_own_lines() {
        let media = PreparedMedia { kind: ItemKind::Video, image_paths: vec!["C:\\work\\1\\f_0.jpg".into(), "C:/work/1/f_500.jpg".into()], frame_times_ms: vec![0, 500], thumbnail_path: String::new() };
        let p = vision_prompt(&media).unwrap();
        let lines: Vec<&str> = p.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2], "C:/work/1/f_0.jpg");
        assert!(!lines[0].contains('/') && !lines[1].contains('/'));
        // The CLI's own pattern for finding image paths must match each path line and nothing else.
        let cli = Regex::new(r"(?:[a-zA-Z]:)?(?:\./|/|\\)[\S\\ ]+?\.(?i:jpg|jpeg|png|webp|mp3|wav)\b").unwrap();
        let found: Vec<&str> = cli.find_iter(&p).map(|m| m.as_str()).collect();
        assert_eq!(found, vec!["C:/work/1/f_0.jpg", "C:/work/1/f_500.jpg"]);
    }

    #[test]
    fn builds_infer_arguments() {
        let cli = GenieCli::new("geniex".into(), "npu".into(), "/tmp/x".into());
        let req = InferRequest { model: "LiquidAI/LFM3-VL-2B-GGUF:Q4_0", system: "sys", user: "u", images: &[], grammar: Some("g"), max_tokens: 400, nctx: 16384 };
        let args = cli.infer_args(&req, "/tmp/x/prompt.txt", Some("/tmp/x/grammar.gbnf"), true);
        let joined = args.join(" ");
        assert!(joined.starts_with("--skip-update --verbose --test-mode infer LiquidAI/LFM3-VL-2B-GGUF:Q4_0 --compute npu --nctx 16384 --max-tokens 400 --think=false"));
        assert!(joined.contains("--min-p 0.000001") && joined.contains("--grammar-path /tmp/x/grammar.gbnf"));
        assert!(joined.ends_with("-s sys -i /tmp/x/prompt.txt"));
    }

    /// Runs a real child process that behaves like `geniex infer --verbose`.
    #[cfg(unix)]
    #[test]
    fn drives_a_child_process_and_streams() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("geniex");
        let body = format!("#!/bin/sh\ncat <<'EOF'\n{VERBOSE_OUT}EOF\n");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cli = Arc::new(GenieCli::new(script.to_string_lossy().to_string(), "npu".into(), d.path().join("scratch").to_string_lossy().to_string()));
        let model = GenieVisionModel::new(cli.clone(), LFM3_VL_2B);
        let media = PreparedMedia { kind: ItemKind::Photo, image_paths: vec!["/tmp/a/image.jpg".into()], frame_times_ms: vec![], thumbnail_path: String::new() };
        let mut streamed = String::new();
        let reply = model.generate(&media, &mut |t| streamed.push_str(t)).unwrap();
        assert_eq!(reply.profile.stop_reason, "eos");
        assert!(streamed.starts_with("{\"description\"") && !streamed.contains("total time"));
        assert_eq!(std::fs::read_to_string(d.path().join("scratch/prompt.txt")).unwrap(), "Describe this photo.\n/tmp/a/image.jpg");
        assert!(cli.last_command.lock().contains("infer LiquidAI/LFM3-VL-2B-GGUF:Q4_0"));
        // A program that prints no profile is a failure with its own words.
        std::fs::write(&script, "#!/bin/sh\necho 'Error: Oops. Model failed to load.' >&2\nexit 1\n").unwrap();
        let err = model.generate(&media, &mut |_| {}).unwrap_err();
        assert!(err.0.contains("Model failed to load"));
    }
}
