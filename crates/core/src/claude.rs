//! Optional cloud help with sorting: Claude through the Messages API over plain HTTPS (there is no
//! official Rust SDK). Text only: the caller sends ids, descriptions, tags and category names,
//! never media, paths or file names. The API key never reaches a log or an error message.

use crate::models::{ReplyFormat, SortUnavailable, TextError, TextModel, TextReply};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MODEL: &str = "claude-sonnet-5-5";
/// Thinking shares this cap, hence its size.
pub const MAX_TOKENS: u32 = 16_000;
/// One non-streaming reply of up to [`MAX_TOKENS`] must arrive within this.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
/// Server-side refusal fallbacks, routed by refusal category.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_SERVER_MESSAGE: usize = 300;

/// Keywords Claude's structured outputs reject with a 400: numeric and string bounds, and array
/// bounds other than `minItems` 0 or 1. The validator still enforces them.
const UNSUPPORTED: [&str; 11] =
    ["maxItems", "minLength", "maxLength", "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "multipleOf", "uniqueItems", "minProperties", "maxProperties"];
/// Maps whose keys are names (of properties or definitions), not keywords.
const NAME_MAPS: [&str; 3] = ["properties", "$defs", "definitions"];

/// `schema` without the keywords Claude does not accept; property names are never touched.
pub fn wire_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, v)| !UNSUPPORTED.contains(&k.as_str()) && !(k.as_str() == "minItems" && v.as_i64().is_some_and(|n| n > 1)))
                .map(|(k, v)| {
                    let cleaned = match v {
                        Value::Object(inner) if NAME_MAPS.contains(&k.as_str()) => Value::Object(inner.iter().map(|(n, s)| (n.clone(), wire_schema(s))).collect()),
                        other => wire_schema(other),
                    };
                    (k.clone(), cleaned)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(wire_schema).collect()),
        other => other.clone(),
    }
}

/// The request body: no thinking or sampling parameters (this model rejects them), medium effort,
/// and the reply held to the JSON schema.
pub fn request_body(system: &str, user: &str, format: &ReplyFormat) -> Value {
    json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": [{"role": "user", "content": user}],
        "output_config": {
            "effort": "medium",
            "format": {"type": "json_schema", "schema": wire_schema(&format.json_schema)}
        },
        "fallbacks": "default"
    })
}

/// The reason behind a failed call, from the HTTP status and the error type the API returned.
pub fn map_status(status: u16, error_type: Option<&str>) -> SortUnavailable {
    match status {
        401 | 403 => SortUnavailable::BadApiKey,
        429 => SortUnavailable::RateLimited,
        // A 400 (a request shape the API rejects, a low credit balance) or a billing error: the
        // same call fails again until something changes, so it never reads as a passing problem.
        400 | 402 => SortUnavailable::RequestRejected,
        _ if error_type == Some("billing_error") => SortUnavailable::RequestRejected,
        // 5xx (529 overloaded) and 4xx we didn't expect: nothing the user can fix.
        _ => SortUnavailable::ServiceError,
    }
}

/// Reads a successful response body into a reply; a refusal is its own outcome.
pub fn read_reply(body: &Value, millis: i64) -> Result<TextReply, TextError> {
    // stop_reason before content: a refusal may come with no content or a partial one.
    let stop = body.get("stop_reason").and_then(Value::as_str).unwrap_or("").to_string();
    if stop == "refusal" {
        let category = body.get("stop_details").and_then(|d| d.get("category")).and_then(Value::as_str).unwrap_or("none");
        return Err(TextError::Unavailable(SortUnavailable::Refused, format!("Claude declined the request (category: {category})")));
    }
    let text: String = body
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("text")).filter_map(|b| b.get("text").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    let usage = |k: &str| body.get("usage").and_then(|u| u.get(k)).and_then(Value::as_i64).unwrap_or(0);
    Ok(TextReply { text, stop_reason: stop, input_tokens: usage("input_tokens"), output_tokens: usage("output_tokens"), millis })
}

/// A refused call as the pipeline sees it: the reason from the status, and a line for the log
/// with the status, the API's error type and its message cut short, never the key.
pub fn status_error(status: u16, body: &Value, key: &str) -> TextError {
    let error = body.get("error");
    let error_type = error.and_then(|e| e.get("type")).and_then(Value::as_str);
    let message: String = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(|m| hide(m, key).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_SERVER_MESSAGE).collect())
        .unwrap_or_default();
    TextError::Unavailable(map_status(status, error_type), format!("Claude call failed ({status} {}): {message}", error_type.unwrap_or("")))
}

/// A call that got no HTTP answer. A connection that never opened is no network; a reply that
/// didn't arrive in time is a timeout.
pub fn transport_error(text: &str, key: &str) -> TextError {
    let text = hide(text, key);
    let lower = text.to_lowercase();
    let reason = if lower.contains("timed out") && !lower.contains("connect") { SortUnavailable::TimedOut } else { SortUnavailable::NoNetwork };
    TextError::Unavailable(reason, format!("Claude call failed: {text}"))
}

fn hide(text: &str, key: &str) -> String {
    if key.is_empty() {
        text.to_string()
    } else {
        text.replace(key, "[key]")
    }
}

/// Claude for sorting. Each call reads the current key, so a key changed in Settings applies to
/// the next call. The call is paid: it is never retried here; the next run tries again.
pub struct ClaudeTextModel {
    api_key: Box<dyn Fn() -> Option<String> + Send + Sync>,
    cancelled: AtomicBool,
}

impl ClaudeTextModel {
    pub fn new(api_key: impl Fn() -> Option<String> + Send + Sync + 'static) -> Self {
        Self { api_key: Box::new(api_key), cancelled: AtomicBool::new(false) }
    }
}

impl TextModel for ClaudeTextModel {
    fn model_id(&self) -> String {
        MODEL.into()
    }

    fn load(&self) -> Result<(), TextError> {
        self.cancelled.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn complete(&self, system: &str, user: &str, format: &ReplyFormat, _max_tokens: u32) -> Result<TextReply, TextError> {
        let key = (self.api_key)().filter(|k| !k.trim().is_empty()).ok_or_else(|| TextError::Unavailable(SortUnavailable::NoApiKey, "No Claude API key is saved".into()))?;
        let cancelled = |started: Instant| TextReply { text: String::new(), stop_reason: "user".into(), input_tokens: 0, output_tokens: 0, millis: started.elapsed().as_millis() as i64 };
        let started = Instant::now();
        if self.cancelled.load(Ordering::SeqCst) {
            return Ok(cancelled(started));
        }
        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout(REQUEST_TIMEOUT).build();
        let result = agent
            .post(API_URL)
            .set("x-api-key", key.trim())
            .set("anthropic-version", API_VERSION)
            .set("anthropic-beta", FALLBACK_BETA)
            .set("content-type", "application/json")
            .send_json(request_body(system, user, format));
        // A blocking request can't be interrupted; a cancel during it discards the reply.
        if self.cancelled.load(Ordering::SeqCst) {
            return Ok(cancelled(started));
        }
        let millis = started.elapsed().as_millis() as i64;
        match result {
            Ok(response) => {
                let body: Value = response.into_json().map_err(|e| TextError::Unavailable(SortUnavailable::ServiceError, format!("Claude's reply could not be read: {e}")))?;
                read_reply(&body, millis)
            }
            Err(ureq::Error::Status(status, response)) => Err(status_error(status, &response.into_json().unwrap_or(Value::Null), key.trim())),
            Err(ureq::Error::Transport(t)) => Err(transport_error(&t.to_string(), key.trim())),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "claude_android_cases.rs"]
mod android_cases;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sorting::schema;

    #[test]
    fn strips_unsupported_schema_keywords() {
        let wire = wire_schema(&schema::assign_json_schema());
        let text = wire.to_string();
        assert!(!text.contains("maxItems"));
        assert!(text.contains("additionalProperties") && text.contains("new_categories"));
        let named = wire_schema(&json!({"properties": {"maxItems": {"type": "string", "maxLength": 4}}, "minItems": 2}));
        assert_eq!(named, json!({"properties": {"maxItems": {"type": "string"}}}));
    }

    #[test]
    fn builds_the_request() {
        let body = request_body("sys", "user", &ReplyFormat { json_schema: schema::propose_json_schema(), grammar: String::new() });
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("temperature").is_none() && body.get("thinking").is_none());
    }

    #[test]
    fn reads_replies_and_maps_errors() {
        let ok = json!({"stop_reason": "end_turn", "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": "{\"a\":1}"}], "usage": {"input_tokens": 12, "output_tokens": 5}});
        let r = read_reply(&ok, 9).unwrap();
        assert_eq!((r.text.as_str(), r.stop_reason.as_str(), r.input_tokens, r.output_tokens), ("{\"a\":1}", "end_turn", 12, 5));
        let refused = json!({"stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}, "content": []});
        assert!(matches!(read_reply(&refused, 1), Err(TextError::Unavailable(SortUnavailable::Refused, _))));
        assert_eq!(map_status(401, None), SortUnavailable::BadApiKey);
        assert_eq!(map_status(429, None), SortUnavailable::RateLimited);
        assert_eq!(map_status(400, Some("invalid_request_error")), SortUnavailable::RequestRejected);
        assert_eq!(map_status(529, Some("overloaded_error")), SortUnavailable::ServiceError);
        let m = ClaudeTextModel::new(|| None);
        assert!(matches!(m.complete("s", "u", &ReplyFormat { json_schema: json!({}), grammar: String::new() }, 10), Err(TextError::Unavailable(SortUnavailable::NoApiKey, _))));
    }
}
