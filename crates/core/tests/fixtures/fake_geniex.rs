//! Stands in for the GenieX CLI in tests: answers the commands the app runs, in the CLI's shapes.
//!
//! It checks the things `geniex infer` itself would reject, so a test fails if the app builds a
//! command the real CLI would not accept (see cli/cmd/geniex/infer.go in qualcomm/GenieX).
//! It uses only the standard library, so the end-to-end test compiles it with plain `rustc` and
//! needs no Python or shell script, on Windows or anywhere else.

use std::io::Write;
use std::process::exit;

fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    exit(1)
}

/// The CLI's own idea of an image path in the prompt: absolute, ending in an image extension.
fn is_image_path(line: &str) -> bool {
    let t = line.trim();
    let bytes = t.as_bytes();
    let absolute = t.starts_with('/') || t.starts_with("./") || t.starts_with('\\') || (bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':');
    let lower = t.to_lowercase();
    absolute && [".jpg", ".jpeg", ".png", ".webp", ".mp3", ".wav"].iter().any(|e| lower.ends_with(e))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(log) = std::env::var("FAKE_GENIEX_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let list: Vec<String> = args.iter().map(|a| json(a)).collect();
            let _ = writeln!(f, "[{}]", list.join(", "));
        }
    }
    let has = |a: &str| args.iter().any(|x| x == a);
    let value = |flag: &str| args.iter().position(|x| x == flag).and_then(|i| args.get(i + 1)).cloned();

    if has("--version") {
        println!("geniex version v0.10.0-fake");
        return;
    }
    if has("list") {
        println!(
            "[{{\"name\": \"LiquidAI/LFM3-VL-2B-GGUF\", \"size\": 1500000000, \"runtime\": \"llama_cpp\", \"type\": \"vlm\", \"precisions\": [\"Q4_0\"]}}, \
             {{\"name\": \"LiquidAI/LFM3-1.7B-GGUF\", \"size\": 1050000000, \"runtime\": \"llama_cpp\", \"type\": \"llm\", \"precisions\": [\"Q4_0\"]}}]"
        );
        return;
    }
    if !has("infer") {
        fail("Error: unknown command");
    }
    let model = args[args.iter().position(|x| x == "infer").unwrap() + 1].clone();
    for flag in ["--compute", "--nctx", "--max-tokens", "--temperature", "--top-k", "--top-p", "--min-p", "--repetition-penalty", "--grammar-path", "-i"] {
        if value(flag).is_none() {
            fail(&format!("Error: missing {flag}"));
        }
    }
    let min_p: f64 = value("--min-p").unwrap().parse().unwrap_or(0.0);
    if !has("--think=false") || min_p <= 0.0 {
        fail("Error: thinking must be off and min-p above zero");
    }
    let grammar = std::fs::read_to_string(value("--grammar-path").unwrap()).unwrap_or_default();
    if !grammar.starts_with("root") {
        fail("Error: bad grammar");
    }
    let prompt = std::fs::read_to_string(value("-i").unwrap()).unwrap_or_default().trim().to_string();
    for (i, line) in prompt.split('\n').enumerate() {
        println!("{}{}", if i == 0 { "> " } else { ". " }, line);
    }

    let reply = if model.contains("VL") {
        let mut images = 0;
        let mut text_lines: Vec<&str> = Vec::new();
        for line in prompt.split('\n') {
            if is_image_path(line) {
                images += 1;
                if !std::path::Path::new(line.trim()).is_file() {
                    println!("Error: file not found: {}", line.trim());
                    exit(1);
                }
            } else {
                text_lines.push(line);
            }
        }
        let text = text_lines.join("\n").trim().to_string();
        if images == 0 || text.contains('/') || value("-s").is_none() {
            fail("Error: Oops. No image or no system prompt.");
        }
        let video = text.starts_with("Video:");
        let actions = if video { "[{\"t\": \"0:00\", \"text\": \"It starts\"}, {\"t\": \"0:02\", \"text\": \"It goes on\"}]" } else { "[]" };
        format!(
            "{{\"description\": \"A real-looking reply for {images} image(s).\", \"actions\": {actions}, \"tags\": [\"fake\", \"geniex\", \"{}\"], \"suggested_name\": \"{}\", \"shot_type\": \"wide\"}}",
            if video { "clip" } else { "still" },
            if video { "Fake Clip" } else { "Fake Still" }
        )
    } else {
        let lines: Vec<&str> = prompt.split('\n').collect();
        let start = lines.iter().position(|l| l.starts_with("Items to sort")).expect("no items to sort");
        let mut items: Vec<String> = Vec::new();
        for line in &lines[start + 1..] {
            if line.trim().is_empty() {
                break;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            let id: i64 = cols[0].trim().parse().expect("item id");
            items.push(format!("{{\"id\": {id}, \"category\": \"Fakes\", \"name\": {}}}", json(&format!("{} sorted", cols[2]))));
        }
        let existing = prompt.contains("Fakes\t");
        let new_categories = if existing { "[]" } else { "[{\"name\": \"Fakes\", \"description\": \"Everything the fake sorted\"}]" };
        format!("{{\"new_categories\": {new_categories}, \"assignments\": [{}]}}", items.join(", "))
    };

    let stdout = std::io::stdout();
    let mut o = stdout.lock();
    let _ = write!(o, "\n{reply}\n\n\n");
    let _ = write!(
        o,
        "total time:     1.000000s\nttft:           0.100000s\nprompt time:    0.100000s\nprompt tokens:  300 token(s)\nprompt speed:   3000.000000 tok/s\ndecode time:    0.900000s\ndecode tokens:  40 token(s)\ndecode speed:   44.400000 tok/s\nstop reason:    eos\n\n"
    );
    let _ = o.flush();
    if has("--test-mode") {
        eprintln!(
            "{{\"Input\": {}, \"Output\": {}, \"Profile\": {{\"TTFT\": 100000, \"MediaTime\": 0, \"PromptTime\": 100000, \"DecodeTime\": 900000, \"PromptTokens\": 300, \"TotalPromptTokens\": 300, \"GeneratedTokens\": 40, \"PrefillSpeed\": 3000.0, \"DecodingSpeed\": 44.4, \"DraftNTotal\": 0, \"DraftNAccepted\": 0, \"StopReason\": \"eos\"}}, \"Error\": null}}",
            json(&prompt),
            json(&reply)
        );
    }
}
