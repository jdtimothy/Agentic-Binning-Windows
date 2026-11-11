//! The models behind analysis and sorting: what they must do, and the simulated ones used when
//! GenieX is not installed. The real backends are in [`crate::geniex`] and [`crate::claude`].

use crate::analysis::{prompt, ItemKind};
use crate::db::Profile;
use crate::media::PreparedMedia;
use crate::paths;
use crate::sorting::parse_reply;
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct VisionReply {
    pub text: String,
    pub profile: Profile,
}

/// The vision model itself failed (not the item): the run stops.
#[derive(Debug, Clone)]
pub struct VisionError(pub String);

/// A vision-language model that analyses one prepared photo or video per call.
pub trait VisionModel: Send + Sync {
    /// Stored with each analysis, e.g. `LiquidAI/LFM3-VL-2B-GGUF:Q4_0`.
    fn model_id(&self) -> String;
    /// Square edge the vision encoder was trained at.
    fn image_edge(&self) -> u32;
    /// Checks the model can run with a context sized for clips of up to `max_frames` frames.
    fn load(&self, max_frames: u32) -> Result<(), VisionError>;
    /// Single-turn analysis of `media`; `on_token` receives each piece of the reply as it streams.
    /// A cancelled call returns with stop reason `user`.
    fn generate(&self, media: &PreparedMedia, on_token: &mut dyn FnMut(&str)) -> Result<VisionReply, VisionError>;
    /// Asks a running `generate` to stop early. Callable from any thread.
    fn cancel(&self);
}

/// How the reply must be shaped, in both forms: Claude reads the JSON schema, the local models
/// the GBNF grammar.
#[derive(Debug, Clone)]
pub struct ReplyFormat {
    pub json_schema: Value,
    pub grammar: String,
}

/// One reply. `stop_reason` is the backend's own word: GenieX gives `eos`, `length` or `user`;
/// Claude gives `end_turn` or `max_tokens`, and `user` when the call was cancelled.
#[derive(Debug, Clone)]
pub struct TextReply {
    pub text: String,
    pub stop_reason: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub millis: i64,
}

/// Why categorizing can't run now; the run ends normally and the items stay uncategorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SortUnavailable {
    NoApiKey,
    NoNetwork,
    BadApiKey,
    RateLimited,
    ServiceError,
    TimedOut,
    RequestRejected,
    Refused,
    ModelNotDownloaded,
    ModelLoadFailed,
}

impl SortUnavailable {
    /// The sentence shown to the user.
    pub fn describe(self) -> &'static str {
        match self {
            SortUnavailable::NoApiKey => "Add a Claude API key in Settings to categorize",
            SortUnavailable::NoNetwork => "No connection; categorizing will try again after the next run",
            SortUnavailable::BadApiKey => "Claude didn't accept the API key; check it in Settings",
            SortUnavailable::RateLimited => "Claude is busy; categorizing will try again after the next run",
            SortUnavailable::ServiceError => "Claude had a problem; categorizing will try again after the next run",
            SortUnavailable::TimedOut => "Claude took too long; categorizing will try again after the next run",
            SortUnavailable::RequestRejected => "Claude rejected the request; check your Claude account, for example its credit balance",
            SortUnavailable::Refused => "Claude declined to categorize some items; they stay uncategorized",
            SortUnavailable::ModelNotDownloaded => "Download the sorting model in Settings",
            SortUnavailable::ModelLoadFailed => "The sorting model couldn't start",
        }
    }
}

#[derive(Debug, Clone)]
pub enum TextError {
    /// The backend can't answer for a reason the user can fix or that will pass. The message is
    /// for the log and never holds the API key.
    Unavailable(SortUnavailable, String),
    /// Anything else.
    Other(String),
}

/// A text-only model that sorts items: a local GenieX model or Claude.
pub trait TextModel: Send + Sync {
    fn model_id(&self) -> String;
    fn load(&self) -> Result<(), TextError>;
    /// One single-turn call. `max_tokens` caps a local model's reply.
    fn complete(&self, system: &str, user: &str, format: &ReplyFormat, max_tokens: u32) -> Result<TextReply, TextError>;
    fn cancel(&self);
}

// ---------------------------------------------------------------------------------------------
// Simulated models
// ---------------------------------------------------------------------------------------------

pub const MOCK_VISION_ID: &str = "simulated/vision";
pub const MOCK_TEXT_ID: &str = "simulated/sorter";

/// Pretends to be the vision model so the whole app can be tried on a computer without GenieX.
/// Its "analysis" is made from the picture's colours and the file's name, and says so.
pub struct MockVisionModel {
    cancelled: Arc<AtomicBool>,
    /// Pause between streamed pieces of the reply.
    pub token_delay: Duration,
}

impl MockVisionModel {
    pub fn new(token_delay: Duration) -> Self {
        Self { cancelled: Arc::new(AtomicBool::new(false)), token_delay }
    }
}

struct Look {
    subject: &'static str,
    category_tag: &'static str,
    setting: &'static str,
    tags: [&'static str; 3],
}

fn look_of(path: &str) -> (Look, &'static str) {
    let img = image::open(path).ok().map(|i| i.thumbnail(32, 32).to_rgb8());
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    if let Some(img) = &img {
        for p in img.pixels() {
            // The letterbox padding is black; it says nothing about the picture.
            if p.0[0] as u32 + p.0[1] as u32 + p.0[2] as u32 > 24 {
                r += p.0[0] as u64;
                g += p.0[1] as u64;
                b += p.0[2] as u64;
                n += 1;
            }
        }
    }
    let n = n.max(1);
    let (r, g, b) = (r / n, g / n, b / n);
    let brightness = (r + g + b) / 3;
    let look = if brightness < 45 {
        Look { subject: "Night scene", category_tag: "night", setting: "a dark scene with little light", tags: ["night", "dark", "low light"] }
    } else if b > r + 12 && b > g {
        Look { subject: "Ocean and sky", category_tag: "ocean", setting: "open water under a wide sky", tags: ["ocean", "sky", "blue"] }
    } else if g > r + 8 && g > b {
        Look { subject: "Green forest", category_tag: "forest", setting: "trees and green leaves", tags: ["forest", "trees", "green"] }
    } else if r > b + 30 && r > g + 8 {
        Look { subject: "Warm sunset", category_tag: "sunset", setting: "warm light low on the horizon", tags: ["sunset", "warm light", "evening"] }
    } else if brightness > 190 {
        Look { subject: "Bright interior", category_tag: "indoor", setting: "a bright, evenly lit space", tags: ["indoor", "bright", "light"] }
    } else {
        Look { subject: "Street view", category_tag: "city", setting: "buildings and muted colours", tags: ["city", "street", "buildings"] }
    };
    let shot = match (r + 2 * g + 3 * b) % 3 {
        0 => "wide",
        1 => "medium",
        _ => "close",
    };
    (look, shot)
}

impl VisionModel for MockVisionModel {
    fn model_id(&self) -> String {
        MOCK_VISION_ID.into()
    }
    fn image_edge(&self) -> u32 {
        crate::analysis::budget::FALLBACK_IMAGE_EDGE
    }
    fn load(&self, _max_frames: u32) -> Result<(), VisionError> {
        self.cancelled.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn generate(&self, media: &PreparedMedia, on_token: &mut dyn FnMut(&str)) -> Result<VisionReply, VisionError> {
        self.cancelled.store(false, Ordering::SeqCst);
        let started = Instant::now();
        let first = media.image_paths.first().ok_or_else(|| VisionError("No image to read".into()))?;
        let (look, shot) = look_of(first);
        let last = media.image_paths.last().map(|p| look_of(p).0);
        let actions: Vec<Value> = if media.kind == ItemKind::Video {
            let end = media.frame_times_ms.last().copied().unwrap_or(0);
            let clock = |ms: i64| format!("{}:{:02}", ms / 60000, (ms / 1000) % 60);
            vec![
                serde_json::json!({"t": "0:00", "text": format!("The clip opens on {}", look.setting)}),
                serde_json::json!({"t": clock(end), "text": format!("It ends on {}", last.as_ref().map(|l| l.setting).unwrap_or(look.setting))}),
            ]
        } else {
            Vec::new()
        };
        let what = if media.kind == ItemKind::Video { "video clip" } else { "photo" };
        let reply = serde_json::json!({
            "description": format!("Simulated analysis: a {what} showing {}. GenieX is not installed, so this text comes from the picture's colours, not from a vision model.", look.setting),
            "actions": actions,
            "tags": [look.category_tag, look.tags[1], look.tags[2], what.split(' ').next().unwrap_or("photo"), "simulated"],
            "suggested_name": look.subject,
            "shot_type": shot,
        })
        .to_string();
        // Stream it a few characters at a time, as the real model streams tokens.
        let chars: Vec<char> = reply.chars().collect();
        let mut sent = String::new();
        let mut tokens = 0;
        for piece in chars.chunks(4) {
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(VisionReply { text: sent, profile: Profile { stop_reason: "user".into(), generated_tokens: tokens, ..Default::default() } });
            }
            let s: String = piece.iter().collect();
            on_token(&s);
            sent.push_str(&s);
            tokens += 1;
            if !self.token_delay.is_zero() {
                std::thread::sleep(self.token_delay);
            }
        }
        let secs = started.elapsed().as_secs_f64().max(0.001);
        Ok(VisionReply {
            text: sent,
            profile: Profile { stop_reason: "eos".into(), ttft_ms: 40.0, prompt_tokens: 256 * media.image_paths.len() as i64 + 180, generated_tokens: tokens, decoding_speed: tokens as f64 / secs },
        })
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Pretends to be the sorting model: files each item under a category made from its first tag.
pub struct MockTextModel {
    cancelled: AtomicBool,
    pub delay: Duration,
}

impl MockTextModel {
    pub fn new(delay: Duration) -> Self {
        Self { cancelled: AtomicBool::new(false), delay }
    }
}

fn category_for(tag: &str) -> &'static str {
    match tag {
        "ocean" => "Ocean",
        "forest" => "Nature",
        "sunset" => "Sunsets",
        "night" => "Night",
        "indoor" => "Indoors",
        "city" => "City",
        _ => "Scenes",
    }
}

impl TextModel for MockTextModel {
    fn model_id(&self) -> String {
        MOCK_TEXT_ID.into()
    }
    fn load(&self) -> Result<(), TextError> {
        self.cancelled.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn complete(&self, _system: &str, user: &str, _format: &ReplyFormat, _max_tokens: u32) -> Result<TextReply, TextError> {
        std::thread::sleep(self.delay);
        if self.cancelled.load(Ordering::SeqCst) {
            return Ok(TextReply { text: String::new(), stop_reason: "user".into(), input_tokens: 0, output_tokens: 0, millis: 0 });
        }
        if user.starts_with("Propose categories") {
            let text = serde_json::json!({"new_categories": [
                {"name": "Ocean", "description": "Water, waves and coastlines"},
                {"name": "Nature", "description": "Forests, plants and landscapes"},
                {"name": "Sunsets", "description": "Warm light at dusk or dawn"},
                {"name": "City", "description": "Streets and buildings"},
            ]})
            .to_string();
            return Ok(TextReply { text, stop_reason: "eos".into(), input_tokens: 0, output_tokens: 0, millis: 1 });
        }
        // Item lines sit between the "Items to sort" header and the next blank line.
        let mut new_categories: Vec<&'static str> = Vec::new();
        let mut assignments = Vec::new();
        let existing: Vec<&str> = user
            .lines()
            .skip(1)
            .take_while(|l| !l.is_empty())
            .filter_map(|l| l.split('\t').next())
            .collect();
        let items = user.lines().skip_while(|l| !l.starts_with("Items to sort")).skip(1).take_while(|l| !l.is_empty());
        for line in items {
            let cols: Vec<&str> = line.split('\t').collect();
            let (Some(id), Some(name), Some(tags)) = (cols.first().and_then(|c| c.parse::<i64>().ok()), cols.get(2), cols.get(3)) else { continue };
            let category = category_for(tags.split(',').next().unwrap_or("").trim());
            if !existing.contains(&category) && !new_categories.contains(&category) {
                new_categories.push(category);
            }
            assignments.push(serde_json::json!({"id": id, "category": category, "name": name}));
        }
        let text = serde_json::json!({
            "new_categories": new_categories.iter().map(|c| serde_json::json!({"name": c, "description": format!("Simulated category for {} shots", c.to_lowercase())})).collect::<Vec<_>>(),
            "assignments": assignments,
        })
        .to_string();
        debug_assert!(parse_reply(&text).is_some());
        Ok(TextReply { text, stop_reason: "eos".into(), input_tokens: 0, output_tokens: 0, millis: self.delay.as_millis() as i64 })
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// The text of the analysis prompt as the model receives it, for the log.
pub fn describe_prompt(media: &PreparedMedia) -> String {
    format!("{} + {} image(s): {}", prompt::user_text(media.kind, &media.frame_times_ms), media.image_paths.len(), media.image_paths.iter().map(|p| paths::file_name(p)).collect::<Vec<_>>().join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis;
    use crate::sorting::{self, CategoryInfo, SortItem};

    #[test]
    fn mock_vision_replies_parse() {
        let d = tempfile::tempdir().unwrap();
        let p = paths::norm(d.path().join("blue.png"));
        image::RgbImage::from_pixel(64, 64, image::Rgb([20, 60, 200])).save(&p).unwrap();
        let m = MockVisionModel::new(Duration::ZERO);
        let media = PreparedMedia { kind: ItemKind::Video, image_paths: vec![p.clone(), p.clone()], frame_times_ms: vec![0, 61_000], thumbnail_path: p };
        let mut streamed = String::new();
        let reply = m.generate(&media, &mut |t| streamed.push_str(t)).unwrap();
        assert_eq!(streamed, reply.text);
        let r = analysis::parse(&reply.text, ItemKind::Video).unwrap();
        assert_eq!(r.suggested_name, "Ocean and sky");
        assert_eq!(r.actions[1].t, "1:01");
        assert!(r.tags.contains(&"simulated".to_string()));
    }

    #[test]
    fn mock_sorter_assigns_every_item() {
        let items: Vec<SortItem> = (1..=3)
            .map(|id| SortItem { id, kind: ItemKind::Photo, suggested_name: format!("Shot {id}"), tags: vec![if id == 3 { "forest".into() } else { "ocean".into() }], description: "d".into(), actions: vec![], name_set_by_user: false })
            .collect();
        let cats = vec![CategoryInfo { id: 1, name: "Ocean".into(), description: "".into(), created_by: "model".into(), items: 0 }];
        let user = sorting::prompt::assign_user(&items, &cats, &[], 12);
        let m = MockTextModel::new(Duration::ZERO);
        let reply = m.complete(sorting::prompt::SYSTEM, &user, &ReplyFormat { json_schema: Value::Null, grammar: String::new() }, 100).unwrap();
        let parsed = parse_reply(&reply.text).unwrap();
        assert_eq!(parsed.assignments.len(), 3);
        assert_eq!(parsed.new_categories.len(), 1);
        assert_eq!(parsed.new_categories[0].name, "Nature");
    }
}
