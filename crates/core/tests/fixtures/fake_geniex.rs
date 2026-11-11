//! Stands in for the GenieX CLI in tests: answers the commands the app runs, in the CLI's shapes.
//!
//! It checks the things `geniex infer` itself would reject, so a test fails if the app builds a
//! command the real CLI would not accept (see cli/cmd/geniex/infer.go in qualcomm/GenieX).
//! `geniex serve` is a small web server here too, answering in the shapes v0.10.0 was seen to use.
//! Every command, and every request the server gets, is written to `geniex-calls.log` beside it.
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

/// Adds a line to `geniex-calls.log` beside this program, where the test reads it.
fn log_line(line: &str) {
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("geniex-calls.log")) {
        let _ = writeln!(f, "{line}");
    }
}

/// Every string value stored under `key` in `body`, in order. Enough JSON for the requests the
/// app sends: a quote inside a string is escaped, so `"key":"` only ever starts a real value.
fn strings_for(body: &str, key: &str) -> Vec<String> {
    let marker = format!("\"{key}\":\"");
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(&marker) {
        rest = &rest[at + marker.len()..];
        let mut value = String::new();
        let mut chars = rest.char_indices();
        let mut end = rest.len();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    end = i + 1;
                    break;
                }
                '\\' => match chars.next().map(|(_, e)| e) {
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    Some('r') => value.push('\r'),
                    Some('u') => {
                        let hex: String = (0..4).filter_map(|_| chars.next().map(|(_, h)| h)).collect();
                        value.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('?'));
                    }
                    Some(other) => value.push(other),
                    None => {}
                },
                c => value.push(c),
            }
        }
        found.push(value);
        rest = &rest[end..];
    }
    found
}

/// The vision model's reply for `images` attached pictures and the instruction `text`.
fn vision_reply(images: usize, text: &str) -> String {
    let video = text.starts_with("Video:");
    let actions = if video { "[{\"t\": \"0:00\", \"text\": \"It starts\"}, {\"t\": \"0:02\", \"text\": \"It goes on\"}]" } else { "[]" };
    format!(
        "{{\"description\": \"A real-looking reply for {images} image(s).\", \"actions\": {actions}, \"tags\": [\"fake\", \"geniex\", \"{}\"], \"suggested_name\": \"{}\", \"shot_type\": \"wide\"}}",
        if video { "clip" } else { "still" },
        if video { "Fake Clip" } else { "Fake Still" }
    )
}

/// The sorting model's reply: every item listed in `prompt` goes into one category.
fn sort_reply(prompt: &str) -> String {
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
}

/// Answers one chat request the way `geniex serve` v0.10.0 does: a status and a body. It refuses
/// what the real server would not act on as the app means it.
fn chat(body: &str) -> (u16, String, bool) {
    let refuse = |why: &str| (400, format!("{{\"error\":{}}}", json(why)), false);
    let Some(model) = strings_for(body, "model").into_iter().next() else { return refuse("no model") };
    if !model.ends_with(":Q4_0") || !body.contains("\"enable_think\":false") || !body.contains("\"nctx\":") || !body.contains("\"compute\":\"") {
        return refuse("the model, thinking off, the context size and the compute unit must all be sent");
    }
    if body.contains("\"messages\":[]") {
        return (200, "null".into(), false); // Only loads the model.
    }
    for field in ["\"stream\":true", "\"max_tokens\":", "\"temperature\":", "\"top_k\":", "\"top_p\":", "\"min_p\":", "\"repetition_penalty\":", "\"grammar_string\":\"root"] {
        if !body.contains(field) {
            return refuse(&format!("missing {field}"));
        }
    }
    if body.contains("\"min_p\":0,") || body.contains("\"min_p\":0.0,") {
        return refuse("min_p of zero means the default");
    }
    let contents = strings_for(body, "content");
    if model.contains("VL") {
        let images = strings_for(body, "url");
        for image in &images {
            if !std::path::Path::new(image).is_file() {
                return refuse(&format!("open {image}: The system cannot find the file specified."));
            }
        }
        let text = strings_for(body, "text").into_iter().next().unwrap_or_default();
        if images.is_empty() || text.is_empty() || contents.len() != 1 {
            return refuse("a picture, the instruction and a system prompt are all needed");
        }
        (200, vision_reply(images.len(), &text), true)
    } else {
        let Some(prompt) = contents.last().filter(|_| contents.len() == 2) else { return refuse("a system prompt and a user prompt are needed") };
        (200, sort_reply(prompt), true)
    }
}

/// `geniex serve`: answers `/health` and `/v1/chat/completions` until it is ended.
fn serve(host: &str) -> ! {
    use std::io::Read;
    let listener = std::net::TcpListener::bind(host).unwrap_or_else(|e| fail(&format!("Error: cannot listen on {host}: {e}")));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut data: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        let (head, body) = loop {
            let split = data.windows(4).position(|w| w == b"\r\n\r\n");
            if let Some(at) = split {
                let head = String::from_utf8_lossy(&data[..at]).to_string();
                let length: usize = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).unwrap_or(0);
                if data.len() >= at + 4 + length {
                    break (head, String::from_utf8_lossy(&data[at + 4..at + 4 + length]).to_string());
                }
            }
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break (String::new(), String::new()),
                Ok(n) => data.extend_from_slice(&buf[..n]),
            }
        };
        let request = head.lines().next().unwrap_or("").to_string();
        let plain = |status: u16, body: &str| format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        if request.starts_with("GET /health") {
            let _ = stream.write_all(plain(200, "{\"status\":\"ok\"}").as_bytes());
        } else if request.starts_with("POST /v1/chat/completions") {
            log_line(&format!("request: {body}"));
            let (status, reply, streamed) = chat(&body);
            if !streamed {
                let _ = stream.write_all(plain(status, &reply).as_bytes());
                continue;
            }
            let mut out = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream;charset=utf-8\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n");
            let chars: Vec<char> = reply.chars().collect();
            for piece in chars.chunks(7) {
                out.push_str(&format!("data:{{\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{}}},\"finish_reason\":null}}]}}\n\n", json(&piece.iter().collect::<String>())));
            }
            out.push_str("data:{\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
            out.push_str("data:{\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"completion_tokens\":40,\"prompt_tokens\":300,\"total_tokens\":340},\"timings\":{\"prompt_n\":300,\"predicted_n\":40,\"predicted_per_second\":44.4}}\n\n");
            out.push_str("data:[DONE]\n\n");
            let _ = stream.write_all(out.as_bytes());
        } else {
            let _ = stream.write_all(plain(404, "{\"error\":\"not found\"}").as_bytes());
        }
    }
    exit(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log_line(&format!("[{}]", args.iter().map(|a| json(a)).collect::<Vec<_>>().join(", ")));
    let has = |a: &str| args.iter().any(|x| x == a);
    let value = |flag: &str| args.iter().position(|x| x == flag).and_then(|i| args.get(i + 1)).cloned();
    if has("serve") {
        // A file named `no-serve` beside this program makes it an older GenieX without the command.
        let old = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("no-serve").exists())).unwrap_or(false);
        match (old, value("--host"), value("--compute"), value("--keepalive")) {
            (false, Some(host), Some(_), Some(_)) => serve(&host),
            (true, ..) => fail("Error: unknown command \"serve\" for \"geniex\""),
            _ => fail("Error: serve needs --host, --compute and --keepalive"),
        }
    }

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
        vision_reply(images, &text)
    } else {
        sort_reply(&prompt)
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
