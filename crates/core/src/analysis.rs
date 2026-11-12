//! The on-device analysis call: prompt copy, output grammar, reply parsing, and video frame timing.
//! Ported from the Android app's `analysis` package; the prompt text is identical.

use crate::names::clean_name;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemKind {
    Photo,
    Video,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Photo => "Photo",
            ItemKind::Video => "Video",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Photo" => Some(ItemKind::Photo),
            "Video" => Some(ItemKind::Video),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionEntry {
    pub t: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnalysisResult {
    pub description: String,
    pub actions: Vec<ActionEntry>,
    pub tags: Vec<String>,
    pub suggested_name: String,
    pub shot_type: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialAnalysis {
    pub description: String,
    pub description_done: bool,
    pub actions: Vec<ActionEntry>,
    pub tags: Vec<String>,
}

/// Fixed prompt copy and output grammar. Changing any text here requires bumping [`PROMPT_VERSION`].
pub mod prompt {
    use super::ItemKind;

    /// `v3`: the main call no longer writes a video's actions. The app picks the moments itself
    /// (see [`super::moments`]) and asks about each in a short call of its own, because the model
    /// labelled its moments with the first few timestamps whatever the clip's length. `cli` marks
    /// that frames reach the model through GenieX's chat template, which places every image
    /// ahead of the text. Android's version is `analysis-v2`.
    pub const PROMPT_VERSION: &str = "analysis-v3-cli";
    pub const MAX_TOKENS_PHOTO: u32 = 400;
    pub const MAX_TOKENS_VIDEO: u32 = 520;
    pub const MAX_TOKENS_MOMENT: u32 = 40;

    pub const SYSTEM: &str = "You catalogue a personal photo and video library. Look carefully and report only what is visible.\n\
Reply with one JSON object and nothing else, with these keys:\n\
\"description\": 1-3 plain sentences: the subject, the setting, and anything notable.\n\
\"actions\": always [].\n\
\"tags\": 5-10 short lowercase keywords for searching (subjects, setting, activity, time of day).\n\
\"suggested_name\": 2-5 words naming the shot, like \"Sea lions on the rocks\". No file extension, no date.\n\
\"shot_type\": \"wide\", \"medium\" or \"close\".";

    pub fn instruction(kind: ItemKind) -> &'static str {
        match kind {
            ItemKind::Photo => "Describe this photo.",
            ItemKind::Video => "Describe this video clip and how it changes over time.",
        }
    }

    pub fn max_tokens(kind: ItemKind) -> u32 {
        match kind {
            ItemKind::Photo => MAX_TOKENS_PHOTO,
            ItemKind::Video => MAX_TOKENS_VIDEO,
        }
    }

    const ROOT: &str = r#"root    ::= "{" ws "\"description\":" ws str "," ws "\"actions\": []," ws "\"tags\":" ws strs "," ws "\"suggested_name\":" ws str "," ws "\"shot_type\":" ws shot ws "}""#;

    const SHARED: &str = r#"strs    ::= "[" ws str ( "," ws str ){2,9} ws "]"
shot    ::= "\"wide\"" | "\"medium\"" | "\"close\""
str     ::= "\"" ( [^"\\\x7F\x00-\x1F] | "\\" ["\\/bfnrt] )* "\""
ws      ::= [ \t\n]*"#;

    /// GBNF grammar constraining the reply to the JSON shape: `actions` is exactly `[]` for a
    /// photo and for a video (the app adds a video's actions afterwards); 3-10 tags. The `[]` is
    /// fixed text with no free white space before it: given the choice, the model wrote blank
    /// space after `"actions":` for a busy clip until the reply was cut off. Uses llama.cpp's
    /// `{m,n}` repetition.
    pub fn grammar(_kind: ItemKind) -> String {
        [ROOT, SHARED].join("\n")
    }

    /// The user turn's text. Every image is placed ahead of the text, so a frame's time cannot
    /// sit beside it as on Android; the model is told only that the frames are in order.
    pub fn user_text(kind: ItemKind, frame_times_ms: &[i64]) -> String {
        match kind {
            ItemKind::Photo => instruction(kind).to_string(),
            ItemKind::Video => format!("Video: the {} images are frames of one clip in time order.\n{}", frame_times_ms.len(), instruction(kind)),
        }
    }

    /// The system prompt of the short call that says what happens at one moment of a clip.
    pub const MOMENT_SYSTEM: &str = "You describe one moment of a video in one short phrase of at most 14 words. Report only what is visible: name the subject, then what it does. Reply with the phrase and nothing else.";

    /// One sentence starting with a capital and ending at its first full stop, where the reply
    /// stops. Ending at a line break instead was tried on real clips: the model never wrote one
    /// and ran on to the token limit, repeating itself. The cost is that "St. Patrick's Day" is
    /// cut at "St".
    pub const MOMENT_GRAMMAR: &str = r#"root ::= [A-Z] [^\n\r"{}.<|]{3,160} ".""#;

    /// The user turn of a moment call. The call gets the one frame at the moment and nothing
    /// about the rest of the clip. Both other ways were tried on real GenieX: given the frame
    /// before as well, the model described that one (a red square, when the moment was the square
    /// turning blue); given the clip's description, it repeated the description's ending for a
    /// frame half a second in.
    pub const MOMENT_TEXT: &str = "This image is one frame of a video clip.\nIn one short phrase, say what is happening in it.";
}

/// Which moments of a clip get an action, and the action's time and text. The app decides when;
/// the model only says what. The idea is from the owner's earlier Agentic Video Binning project,
/// which measures motion and ties each model call to a few frames from one stretch of the clip.
pub mod moments {
    /// The opening and up to four later moments.
    pub const MAX_LATER: usize = 4;
    /// Mean change between two frames (0-255, on a 32 px copy) below which nothing happened.
    /// Measured on 14 real clips: a locked-off or gently hand-held shot of something still stays
    /// under 8 (and got the same phrase for every moment); waves breaking, a pan to something
    /// new or a person leaving the picture reach 10 and more.
    pub const QUIET: f64 = 8.0;
    /// A later moment must also reach this share of the clip's largest change.
    const SHARE_OF_PEAK: f64 = 0.4;

    /// The frames that get an action, in order. `changes[i]` is how much frame `i` differs from
    /// frame `i - 1` (`changes[0]` is unused). A clip in which nothing changes gets none. Otherwise
    /// frame 0 is the opening, and the frames after it are split into up to four equal stretches,
    /// each giving its most-changed frame if that change is large enough. So the moments are
    /// spread over the clip and sit where the picture moves.
    pub fn pick(changes: &[f64]) -> Vec<usize> {
        let n = changes.len();
        if n < 2 {
            return Vec::new();
        }
        let peak = changes[1..].iter().cloned().fold(0.0, f64::max);
        if peak < QUIET {
            return Vec::new();
        }
        let floor = QUIET.max(peak * SHARE_OF_PEAK);
        let later = n - 1;
        let stretches = MAX_LATER.min(later);
        let mut picked = vec![0];
        for s in 0..stretches {
            let from = 1 + later * s / stretches;
            let to = 1 + later * (s + 1) / stretches;
            let best = (from..to).fold(from, |best, i| if changes[i] > changes[best] { i } else { best });
            if changes[best] >= floor {
                picked.push(best);
            }
        }
        picked
    }

    /// `m:ss`, to the nearest second.
    pub fn clock(time_ms: i64) -> String {
        let seconds = (time_ms.max(0) + 500) / 1000;
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }

    /// The model's phrase as an action's text: one line, no quotes or closing full stop. None when
    /// there is no usable phrase.
    pub fn clean(text: &str) -> Option<String> {
        let line = text.trim().lines().next().unwrap_or("");
        // Without the grammar (`geniex infer` on an old CLI) a reply can run into the model's own
        // markers, such as `</think>`.
        let line = line.split(['<', '|']).next().unwrap_or("");
        let line = line.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
        let line = line.trim_end_matches('.').trim();
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() < 2 || !line.chars().any(char::is_alphabetic) {
            return None;
        }
        Some(words.join(" "))
    }

    /// Whether `b` says what `a` already said: at least half of the longer words of the two
    /// phrases are shared. "Aerial view of coastal road with waves crashing against rocky shores"
    /// came back four times in slightly different words for one clip.
    pub fn same_thing(a: &str, b: &str) -> bool {
        let words = |s: &str| -> std::collections::BTreeSet<String> {
            s.split(|c: char| !c.is_alphanumeric()).map(|w| w.to_lowercase()).filter(|w| w.chars().count() > 3).map(|w| w.trim_end_matches('s').to_string()).collect()
        };
        let (a, b) = (words(a), words(b));
        if a.is_empty() || b.is_empty() {
            return a == b;
        }
        let shared = a.intersection(&b).count();
        shared * 2 >= a.len().max(b.len())
    }
}

/// Frame sampling and timestamp rules for video input, matching llama.cpp's video helper for
/// LFM3-VL: frames at 2 fps, spread evenly when a clip would exceed the cap.
pub mod timing {
    /// LFM3-VL was trained on video sampled at 2 fps (model card).
    pub const FPS: f32 = 2.0;
    const TIMESTAMP_INTERVAL_MS: i64 = 5000;

    /// Sample times for a clip: every 1/FPS s from the start. Clips that would exceed
    /// `max_frames` are spread evenly over their full length instead of being cut short.
    /// A non-positive duration yields a single frame at 0.
    pub fn sample_times_ms(duration_ms: i64, max_frames: usize) -> Vec<i64> {
        if duration_ms <= 0 || max_frames == 0 {
            return vec![0];
        }
        let step = (1000.0 / FPS) as i64;
        let at_fps = (((duration_ms + step - 1) / step) as usize).max(1);
        if at_fps <= max_frames {
            return (0..at_fps as i64).map(|i| i * step).collect();
        }
        (0..max_frames as i64).map(|i| duration_ms * i / max_frames as i64).collect()
    }

    /// Indices that get a timestamp after them under llama.cpp's rule: the first frame whose time
    /// reaches each multiple of 5 s, starting with frame 0.
    pub fn timestamped_indices(times_ms: &[i64]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut next = 0i64;
        for (i, t) in times_ms.iter().enumerate() {
            if *t >= next {
                out.push(i);
                next += TIMESTAMP_INTERVAL_MS;
            }
        }
        out
    }

    /// llama.cpp's video helper format: `[%dm%.2fs]`.
    pub fn format(time_ms: i64) -> String {
        let seconds = time_ms as f32 / 1000.0;
        let minutes = (seconds / 60.0) as i32;
        format!("[{}m{:.2}s]", minutes, seconds - (minutes * 60) as f32)
    }

    /// `m:ss` or `m:ss.s`, the form the reply's actions use.
    pub fn clock(time_ms: i64) -> String {
        let total_tenths = (time_ms.max(0) + 50) / 100;
        let minutes = total_tenths / 600;
        let seconds = (total_tenths % 600) / 10;
        let tenths = total_tenths % 10;
        if tenths == 0 {
            format!("{minutes}:{seconds:02}")
        } else {
            format!("{minutes}:{seconds:02}.{tenths}")
        }
    }
}

/// Sizes the vision model's context for the frame cap, and describes what a cap means for a clip.
pub mod budget {
    use super::timing;

    /// What a 16-frame cap needed in the field; never go below it.
    pub const MIN_CONTEXT: u32 = 16384;
    const CONTEXT_STEP: u64 = 4096;
    /// Each frame's timestamp text and media marker, on top of the image tokens.
    const TOKENS_PER_FRAME_EXTRA: u64 = 12;
    /// The system prompt, the instruction and a 520-token reply, with margin.
    const FIXED_TOKENS: u64 = 2048;
    const HINT_CLIP_SECONDS: u32 = 60;
    /// Image tokens per frame when the mmproj does not say (256 at 512 px for LFM3-VL).
    pub const FALLBACK_TOKENS_PER_FRAME: u32 = 256;
    pub const FALLBACK_IMAGE_EDGE: u32 = 512;

    /// `max_frames * (tokens_per_frame + 12) + 2048` rounded up to a multiple of 4096, at least 16384.
    pub fn context_for(max_frames: u32, tokens_per_frame: u32) -> u32 {
        let needed = max_frames as u64 * (tokens_per_frame as u64 + TOKENS_PER_FRAME_EXTRA) + FIXED_TOKENS;
        let rounded = needed.div_ceil(CONTEXT_STEP) * CONTEXT_STEP;
        rounded.max(MIN_CONTEXT as u64).min(u32::MAX as u64) as u32
    }

    /// What the cap does to a 1-minute clip: 2 fps would give 120 frames, so a lower cap spaces them out.
    pub fn hint(max_frames: u32) -> String {
        let frames = (HINT_CLIP_SECONDS * timing::FPS as u32).min(max_frames).max(1);
        format!("1-minute clip: {} frames, one every {:.1} s", frames, HINT_CLIP_SECONDS as f64 / frames as f64)
    }
}

const SHOT_TYPES: [&str; 3] = ["wide", "medium", "close"];
const MAX_TAGS: usize = 10;
const MAX_NAME: usize = 60;

#[derive(Deserialize)]
struct RawAnalysis {
    #[serde(default)]
    description: String,
    #[serde(default)]
    actions: Vec<ActionEntry>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    suggested_name: String,
    #[serde(default = "default_shot")]
    shot_type: String,
}

fn default_shot() -> String {
    "medium".into()
}

/// Parses a finished reply: the outermost `{...}` must be the JSON object the prompt asks for,
/// with a description and a usable name. Null when it is not.
pub fn parse(text: &str, kind: ItemKind) -> Option<AnalysisResult> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let raw: RawAnalysis = serde_json::from_str(&text[start..=end]).ok()?;
    let description = raw.description.trim().to_string();
    if description.is_empty() {
        return None;
    }
    let name = clean_name(&raw.suggested_name, MAX_NAME);
    if name.is_empty() {
        return None;
    }
    let mut tags: Vec<String> = Vec::new();
    for t in raw.tags {
        let t = t.trim().to_lowercase();
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    }
    tags.truncate(MAX_TAGS);
    let shot = raw.shot_type.trim().to_lowercase();
    Some(AnalysisResult {
        description,
        actions: if kind == ItemKind::Video { raw.actions } else { Vec::new() },
        tags,
        suggested_name: name,
        shot_type: if SHOT_TYPES.contains(&shot.as_str()) { shot } else { "medium".into() },
    })
}

struct StrScan {
    value: String,
    end: usize,
    closed: bool,
}

/// Reads a JSON string whose opening quote is at `open` (indices into `s`).
fn scan_string(s: &[char], open: usize) -> StrScan {
    let mut out = String::new();
    let mut i = open + 1;
    while i < s.len() {
        let c = s[i];
        if c == '"' {
            return StrScan { value: out, end: i + 1, closed: true };
        }
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        if i + 1 >= s.len() {
            break;
        }
        match s[i + 1] {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'u' => {
                if i + 6 > s.len() {
                    break;
                }
                let hex: String = s[i + 2..i + 6].iter().collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
                i += 4;
            }
            e => out.push(e),
        }
        i += 2;
    }
    StrScan { value: out, end: s.len(), closed: false }
}

fn skip_ws(s: &[char], from: usize) -> usize {
    let mut i = from;
    while i < s.len() && s[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Index just after `"key":` and any whitespace, or None if the key has not arrived.
fn value_start(s: &[char], key: &str, from: usize) -> Option<usize> {
    let needle: Vec<char> = format!("\"{key}\":").chars().collect();
    if from > s.len() || needle.len() > s.len() {
        return None;
    }
    (from..=s.len() - needle.len()).find(|&i| s[i..i + needle.len()] == needle[..]).map(|k| skip_ws(s, k + needle.len()))
}

/// Index after the `}` closing the object that starts at `open`, or None if it is incomplete.
fn object_end(s: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < s.len() {
        let c = s[i];
        if c == '"' {
            let st = scan_string(s, i);
            if !st.closed {
                return None;
            }
            i = st.end;
            continue;
        }
        if c == '{' {
            depth += 1;
        }
        if c == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

fn parse_action(text: &str) -> Option<ActionEntry> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(ActionEntry { t: v.get("t")?.as_str()?.to_string(), text: v.get("text")?.as_str()?.to_string() })
}

/// Best-effort view of a reply that is still streaming in. Never fails: anything it cannot read
/// yet is simply left empty. The description grows as characters arrive; actions and tags appear
/// only once each entry is complete.
pub fn partial(text: &str) -> PartialAnalysis {
    let s: Vec<char> = text.chars().collect();
    let none = PartialAnalysis::default();
    let Some(d_start) = value_start(&s, "description", 0) else { return none };
    if d_start >= s.len() || s[d_start] != '"' {
        return none;
    }
    let desc = scan_string(&s, d_start);
    if !desc.closed {
        return PartialAnalysis { description: desc.value, ..none };
    }
    let done = PartialAnalysis { description: desc.value.clone(), description_done: true, ..none };
    let Some(a_start) = value_start(&s, "actions", desc.end) else { return done };
    if a_start >= s.len() || s[a_start] != '[' {
        return done;
    }
    let mut actions = Vec::new();
    let mut i = a_start + 1;
    let mut array_closed = false;
    loop {
        i = skip_ws(&s, i);
        if i >= s.len() {
            break;
        }
        match s[i] {
            ',' => {
                i += 1;
            }
            ']' => {
                array_closed = true;
                i += 1;
                break;
            }
            '{' => {
                let Some(end) = object_end(&s, i) else { break };
                let obj: String = s[i..end].iter().collect();
                if let Some(a) = parse_action(&obj) {
                    actions.push(a);
                }
                i = end;
            }
            _ => break,
        }
    }
    let mut tags: Vec<String> = Vec::new();
    if array_closed {
        if let Some(t_start) = value_start(&s, "tags", i) {
            if t_start < s.len() && s[t_start] == '[' {
                let mut j = t_start + 1;
                loop {
                    j = skip_ws(&s, j);
                    if j >= s.len() {
                        break;
                    }
                    if s[j] == ',' {
                        j += 1;
                        continue;
                    }
                    if s[j] != '"' {
                        break;
                    }
                    let tag = scan_string(&s, j);
                    if !tag.closed {
                        break;
                    }
                    let v = tag.value.trim().to_lowercase();
                    if !v.is_empty() && !tags.contains(&v) {
                        tags.push(v);
                    }
                    j = tag.end;
                }
            }
        }
    }
    tags.truncate(MAX_TAGS);
    PartialAnalysis { description: desc.value, description_done: true, actions, tags }
}

#[cfg(test)]
#[path = "analysis_android_cases.rs"]
mod android_cases;

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"Sure: {"description": " A turtle swims. ", "actions": [{"t":"0:00","text":"swims"},{"t":"0:05","text":"dives"}], "tags": ["Turtle", "sea", "turtle", ""], "suggested_name": "Sea: Turtle.", "shot_type": "CLOSE"} done"#;

    #[test]
    fn parses_reply() {
        let r = parse(REPLY, ItemKind::Video).unwrap();
        assert_eq!(r.description, "A turtle swims.");
        assert_eq!(r.actions.len(), 2);
        assert_eq!(r.tags, vec!["turtle", "sea"]);
        assert_eq!(r.suggested_name, "Sea Turtle");
        assert_eq!(r.shot_type, "close");
        assert!(parse(REPLY, ItemKind::Photo).unwrap().actions.is_empty());
        assert!(parse("no json", ItemKind::Photo).is_none());
        assert!(parse(r#"{"description":"","suggested_name":"x"}"#, ItemKind::Photo).is_none());
        assert!(parse(r#"{"description":"x","suggested_name":"??"}"#, ItemKind::Photo).is_none());
        assert_eq!(parse(r#"{"description":"x","suggested_name":"y","shot_type":"odd"}"#, ItemKind::Photo).unwrap().shot_type, "medium");
    }

    #[test]
    fn partial_grows() {
        assert_eq!(partial(""), PartialAnalysis::default());
        let p = partial(r#"{"description": "A tur"#);
        assert_eq!(p.description, "A tur");
        assert!(!p.description_done);
        let p = partial(r#"{"description": "A \"big\" turtle", "actions": [{"t":"0:00","text":"swims"},{"t":"0:0"#);
        assert!(p.description_done);
        assert_eq!(p.description, "A \"big\" turtle");
        assert_eq!(p.actions.len(), 1);
        assert!(p.tags.is_empty());
        let p = partial(r#"{"description": "x", "actions": [], "tags": ["Sea", "tur"#);
        assert_eq!(p.tags, vec!["sea"]);
    }

    #[test]
    fn sample_times() {
        assert_eq!(timing::sample_times_ms(0, 16), vec![0]);
        assert_eq!(timing::sample_times_ms(300, 16), vec![0]);
        assert_eq!(timing::sample_times_ms(2000, 16), vec![0, 500, 1000, 1500]);
        let t = timing::sample_times_ms(60_000, 16);
        assert_eq!(t.len(), 16);
        assert_eq!(t[1], 3750);
        assert_eq!(timing::timestamped_indices(&timing::sample_times_ms(8000, 16)), vec![0, 10]);
        assert_eq!(timing::format(65_500), "[1m5.50s]");
        assert_eq!(timing::clock(65_500), "1:05.5");
        assert_eq!(timing::clock(5000), "0:05");
    }

    #[test]
    fn context_budget() {
        assert_eq!(budget::context_for(16, 256), 16384);
        assert_eq!(budget::context_for(64, 256), 20480);
        assert_eq!(budget::hint(16), "1-minute clip: 16 frames, one every 3.8 s");
    }

    #[test]
    fn grammar_shapes() {
        // A video's actions come from the moment calls, so the main reply has none either, and
        // the grammar leaves no free white space before the [].
        assert_eq!(prompt::grammar(ItemKind::Video), prompt::grammar(ItemKind::Photo));
        assert!(prompt::grammar(ItemKind::Video).contains(r#""\"actions\": [],""#));
        assert!(prompt::user_text(ItemKind::Video, &[0, 500]).starts_with("Video: the 2 images"));
        assert!(prompt::MOMENT_GRAMMAR.starts_with("root"));
        assert!(!prompt::MOMENT_TEXT.contains('/') && !prompt::MOMENT_TEXT.contains('\\'));
    }

    #[test]
    fn moments_sit_where_the_picture_changes() {
        use moments::{clean, clock, pick, same_thing};
        assert!(same_thing("Aerial view of coastal road with waves crashing against rocky shores", "Aerial view of coastal road, ocean waves crashing against rocky shore"));
        assert!(same_thing("A green lizard perches on a pink flower bud", "A small green lizard perches on a pink flower bud"));
        assert!(!same_thing("A person stands on rocks by the sea", "Waves crash against rocks creating splashes"));
        assert!(!same_thing("A man in a white shirt holds a smartphone", "Man with phone moves left out of frame"));
        assert_eq!(clean("A man takes a photo.</think>A man"), Some("A man takes a photo".to_string()));
        // Nothing moves: no actions at all. A real locked-off shot of a tree measured 3 to 7.
        assert!(pick(&[0.0, 3.0, 6.0, 7.0, 5.0]).is_empty());
        assert!(pick(&[0.0, 1.0, 2.5, 0.4]).is_empty());
        assert!(pick(&[0.0]).is_empty() && pick(&[]).is_empty());
        // One event late in a 16-frame clip: the opening and that frame only.
        let mut c = vec![1.0; 16];
        c[13] = 30.0;
        assert_eq!(pick(&c), vec![0, 13]);
        // Steady movement throughout: the opening and one frame from each quarter.
        let mut c = vec![12.0; 16];
        c[2] = 14.0;
        c[7] = 15.0;
        c[9] = 13.0;
        c[15] = 20.0;
        assert_eq!(pick(&c), vec![0, 2, 7, 9, 15]);
        // Two frames: the opening and the second.
        assert_eq!(pick(&[0.0, 9.0]), vec![0, 1]);
        assert_eq!((clock(0), clock(500), clock(18_260), clock(73_900)), ("0:00".to_string(), "0:01".to_string(), "0:18".to_string(), "1:14".to_string()));
        assert_eq!(clean(" \"A turtle swims past.\" \nmore"), Some("A turtle swims past".to_string()));
        assert_eq!(clean("People jog in a park on St. Patrick's Day.\n"), Some("People jog in a park on St. Patrick's Day".to_string()));
        assert_eq!(clean("Waves."), None);
        assert_eq!(clean("..."), None);
    }
}
