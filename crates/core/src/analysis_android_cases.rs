//! The Android app's test cases for the `analysis` package, run against this port:
//! `AnalysisParserTest`, `VideoTimingTest`, `VisionBudgetTest` and `AnalysisPromptTest`.
//! `PromptAssemblyTest` has no counterpart: the GenieX command line places the images itself.

use super::*;

const FULL_VIDEO: &str = r#"{"description":"Sea lions on rocks, \"basking\" in the sun.","actions":[{"t":"0:00","text":"Pan right"},{"t":"0:04","text":"One slides into the water"}],"tags":["sea lions","rocks","sunny"],"suggested_name":"Sea lions on the rocks","shot_type":"wide"}"#;

/// A photo reply with these fields changed: `description`, `actions`, `tags`, `suggested_name`, `shot_type`.
fn reply(change: &[(&str, &str)]) -> String {
    let mut fields = vec![("description", "\"A pier at night.\""), ("actions", "[]"), ("tags", "[\"pier\"]"), ("suggested_name", "\"Pier at night\""), ("shot_type", "\"wide\"")];
    for (key, value) in change {
        fields.iter_mut().find(|(k, _)| k == key).unwrap().1 = value;
    }
    format!("{{{}}}", fields.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect::<Vec<_>>().join(","))
}

#[test]
fn parse_video() {
    let r = parse(
        r#"{"description":"Sea lions on rocks.","actions":[{"t":"0:00","text":"Pan right"}],"tags":["Sea Lions","rocks","sea lions"],"suggested_name":"Sea lions on the rocks","shot_type":"wide"}"#,
        ItemKind::Video,
    )
    .unwrap();
    assert_eq!(r.tags, vec!["sea lions", "rocks"]);
    assert_eq!(r.actions, vec![ActionEntry { t: "0:00".into(), text: "Pan right".into() }]);
    assert_eq!(r.shot_type, "wide");
}

#[test]
fn parse_photo_drops_actions() {
    let r = parse(&reply(&[("actions", r#"[{"t":"0:00","text":"Pan right"}]"#)]), ItemKind::Photo).unwrap();
    assert!(r.actions.is_empty());
}

#[test]
fn parse_cleans_suggested_name() {
    assert_eq!(parse(&reply(&[("suggested_name", "\"Pier: night/moon...\"")]), ItemKind::Photo).unwrap().suggested_name, "Pier night moon");
}

#[test]
fn parse_caps_name_and_tags() {
    let tags = format!("[{}]", (1..=14).map(|i| format!("\"t{i}\"")).collect::<Vec<_>>().join(","));
    let name = format!("\"{}\"", "word ".repeat(30).trim());
    let r = parse(&reply(&[("suggested_name", &name), ("tags", &tags)]), ItemKind::Photo).unwrap();
    assert!(r.suggested_name.chars().count() <= 60);
    assert!(!r.suggested_name.ends_with(' '));
    assert_eq!(r.tags.len(), 10);
}

#[test]
fn parse_rejects_replies_it_cannot_use() {
    // A blank description, a name with nothing left after cleaning, a reply cut short, and prose.
    assert!(parse(&reply(&[("description", "\"  \"")]), ItemKind::Photo).is_none());
    assert!(parse(&reply(&[("suggested_name", "\"?*:.\"")]), ItemKind::Photo).is_none());
    assert!(parse(&FULL_VIDEO[..60], ItemKind::Video).is_none());
    assert!(parse("I cannot help with that.", ItemKind::Photo).is_none());
}

#[test]
fn parse_ignores_text_before_first_brace() {
    assert!(parse(&format!("Here you go:\n{}", reply(&[])), ItemKind::Photo).is_some());
}

#[test]
fn parse_unknown_shot_type_becomes_medium() {
    assert_eq!(parse(&reply(&[("shot_type", "\"panoramic\"")]), ItemKind::Photo).unwrap().shot_type, "medium");
}

#[test]
fn partial_streams_description() {
    let p = partial(r#"{"description":"Golden-hour view acr"#);
    assert_eq!(p.description, "Golden-hour view acr");
    assert!(!p.description_done);
}

#[test]
fn partial_empty_and_garbage() {
    assert_eq!(partial(""), PartialAnalysis::default());
    assert_eq!(partial("{\"desc").description, "");
}

#[test]
fn partial_after_description_collects_completed_actions_and_tags() {
    let p = partial(r#"{"description":"Sea lions.","actions":[{"t":"0:00","text":"Pan right"},{"t":"0:04","text":"One sl"#);
    assert_eq!(p.description, "Sea lions.");
    assert!(p.description_done);
    assert_eq!(p.actions, vec![ActionEntry { t: "0:00".into(), text: "Pan right".into() }]);
    assert!(p.tags.is_empty());
    let q = partial(FULL_VIDEO.split("\"suggested_name\"").next().unwrap());
    assert_eq!(q.actions.len(), 2);
    assert_eq!(q.tags, vec!["sea lions", "rocks", "sunny"]);
}

#[test]
fn partial_mid_tags_returns_only_complete_tags() {
    let p = partial(&format!("{}\"ro", FULL_VIDEO.split("\"rocks\"").next().unwrap()));
    assert_eq!(p.tags, vec!["sea lions"]);
}

#[test]
fn partial_decodes_escapes() {
    assert_eq!(partial(FULL_VIDEO).description, "Sea lions on rocks, \"basking\" in the sun.");
    // A dangling escape at the cut must not fail or leak the backslash.
    assert_eq!(partial("{\"description\":\"a\\").description, "a");
    assert_eq!(partial("{\"description\":\"a\\u00").description, "a");
}

#[test]
fn partial_never_fails_and_description_never_shrinks() {
    let chars: Vec<char> = FULL_VIDEO.chars().collect();
    let mut last = 0;
    for i in 0..=chars.len() {
        let p = partial(&chars[..i].iter().collect::<String>());
        assert!(p.description.chars().count() >= last, "shrank at {i}");
        last = p.description.chars().count();
    }
    let done = partial(FULL_VIDEO);
    assert!(done.description_done);
    assert_eq!((done.actions.len(), done.tags.len()), (2, 3));
}

// ---------------------------------------------------------------------------------------------
// VideoTimingTest
// ---------------------------------------------------------------------------------------------

#[test]
fn sample_times_short_clip_at_2fps() {
    assert_eq!(timing::sample_times_ms(8_000, 16), (0..16).map(|i| i * 500).collect::<Vec<i64>>());
}

#[test]
fn sample_times_long_clip_spread_evenly() {
    assert_eq!(timing::sample_times_ms(14_000, 16), (0..16).map(|i| 14_000 * i / 16).collect::<Vec<i64>>());
}

#[test]
fn sample_times_extreme_durations() {
    let hour = timing::sample_times_ms(3_600_000, 16);
    assert_eq!(hour.len(), 16);
    assert!(*hour.last().unwrap() < 3_600_000);
    for duration in [300, 0, -5] {
        assert_eq!(timing::sample_times_ms(duration, 16), vec![0], "{duration}");
    }
}

#[test]
fn sample_times_cap_is_honoured() {
    assert_eq!(timing::sample_times_ms(60_000, 32).len(), 32);
    assert_eq!(timing::sample_times_ms(60_000, 64).len(), 64);
    // Never faster than 2 fps: a 5 s clip always gets 10 frames.
    for cap in [16, 24, 32, 48, 64] {
        assert_eq!(timing::sample_times_ms(5_000, cap).len(), 10, "{cap}");
    }
}

#[test]
fn timestamps_first_and_every_5s() {
    assert_eq!(timing::timestamped_indices(&timing::sample_times_ms(8_000, 16)), vec![0, 10]);
}

#[test]
fn format_matches_llama_cpp() {
    assert_eq!(timing::format(0), "[0m0.00s]");
    assert_eq!(timing::format(65_500), "[1m5.50s]");
}

// ---------------------------------------------------------------------------------------------
// VisionBudgetTest
// ---------------------------------------------------------------------------------------------

#[test]
fn context_for_frames() {
    assert_eq!(budget::context_for(16, 256), 16384);
    assert_eq!(budget::context_for(64, 256), 20480);
    // Never below 16384.
    assert_eq!(budget::context_for(1, 64), 16384);
    assert_eq!(budget::context_for(24, 256), 16384);
    // 48 * 268 + 2048 = 14912: still the floor.
    assert_eq!(budget::context_for(48, 256), 16384);
    // A bigger tower: 64 * (576 + 12) + 2048 = 39680, rounded up to 40960.
    assert_eq!(budget::context_for(64, 576), 40960);
}

#[test]
fn hints() {
    assert_eq!(budget::hint(32), "1-minute clip: 32 frames, one every 1.9 s");
    assert_eq!(budget::hint(16), "1-minute clip: 16 frames, one every 3.8 s");
    // A cap above 120 is limited to two frames a second.
    assert_eq!(budget::hint(200), "1-minute clip: 120 frames, one every 0.5 s");
}

// ---------------------------------------------------------------------------------------------
// AnalysisPromptTest
// ---------------------------------------------------------------------------------------------

fn rule<'a>(grammar: &'a str, name: &str) -> Option<&'a str> {
    grammar.lines().find(|l| l.starts_with(&format!("{name} "))).map(|l| l.split_once("::= ").unwrap().1)
}

#[test]
fn grammar_and_prompt_mention_same_keys() {
    let (video, photo) = (prompt::grammar(ItemKind::Video), prompt::grammar(ItemKind::Photo));
    let keys: std::collections::BTreeSet<String> = regex::Regex::new(r#""(\w+)":"#).unwrap().captures_iter(prompt::SYSTEM).map(|c| c[1].to_string()).collect();
    // Differs from Android on purpose (analysis-v3-cli): the main reply's actions are always [],
    // for a video too, so the action object's keys `t` and `text` are in neither prompt nor grammar.
    let expected: std::collections::BTreeSet<String> = ["description", "actions", "tags", "suggested_name", "shot_type"].iter().map(|s| s.to_string()).collect();
    assert_eq!(keys, expected);
    for k in &keys {
        let literal = format!("\\\"{k}\\\":");
        assert!(video.contains(&literal), "the video grammar lacks {k}");
        assert!(photo.contains(&literal), "the photo grammar lacks {k}");
    }
}

#[test]
fn grammar_per_kind_actions() {
    // Differs from Android on purpose: Android's video grammar asks for 2-5 actions with m:ss
    // times. Here the app adds a video's actions from its own moment calls (`analysis::moments`).
    for grammar in [prompt::grammar(ItemKind::Video), prompt::grammar(ItemKind::Photo)] {
        assert!(grammar.contains(r#""\"actions\": [],""#));
        assert_eq!(rule(&grammar, "actions"), None);
        assert_eq!(rule(&grammar, "action"), None);
        assert_eq!(rule(&grammar, "ts"), None);
    }
}

#[test]
fn grammar_keeps_shared_rules_verbatim() {
    let shared = [
        r#"strs    ::= "[" ws str ( "," ws str ){2,9} ws "]""#,
        r#"str     ::= "\"" ( [^"\\\x7F\x00-\x1F] | "\\" ["\\/bfnrt] )* "\"""#,
        r"ws      ::= [ \t\n]*",
        r#"shot    ::= "\"wide\"" | "\"medium\"" | "\"close\"""#,
        r#"root    ::= "{" ws "\"description\":" ws str "," ws "\"actions\": []," ws "\"tags\":" ws strs "," ws "\"suggested_name\":" ws str "," ws "\"shot_type\":" ws shot ws "}""#,
    ];
    for (grammar, lines) in [(prompt::grammar(ItemKind::Video), 5), (prompt::grammar(ItemKind::Photo), 5)] {
        for line in shared {
            assert!(grammar.lines().any(|l| l == line), "missing: {line}");
        }
        assert_eq!(grammar.lines().count(), lines);
    }
}

#[test]
fn instructions_and_token_budgets() {
    assert_eq!(prompt::instruction(ItemKind::Photo), "Describe this photo.");
    assert_eq!(prompt::instruction(ItemKind::Video), "Describe this video clip and how it changes over time.");
    assert_eq!((prompt::MAX_TOKENS_PHOTO, prompt::MAX_TOKENS_VIDEO), (400, 520));
    // Android's version is analysis-v2. v3 moved a video's actions to the app's own moment calls;
    // the suffix marks that frames go through GenieX's chat template.
    assert_eq!(prompt::PROMPT_VERSION, "analysis-v3-cli");
}

/// Windows only: the command line finds image paths by pattern, so the text beside them must not
/// look like a path.
#[test]
fn no_prompt_text_holds_a_slash() {
    for kind in [ItemKind::Photo, ItemKind::Video] {
        let text = prompt::user_text(kind, &timing::sample_times_ms(61_000, 64));
        assert!(!text.contains('/') && !text.contains('\\'), "{text}");
    }
}
