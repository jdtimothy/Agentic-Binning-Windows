//! The Android app's `ClaudeErrorMappingTest`, run against this port. No test here sends a
//! request: a paid call is never made in a test. The cases about Android's SDK client (closing
//! it, cancelling its future) have no counterpart: this port makes one blocking HTTPS request.

use super::*;
use crate::sorting::schema;

const SECRET: &str = "sk-ant-test-0123456789";

fn format() -> ReplyFormat {
    ReplyFormat { json_schema: schema::assign_json_schema(), grammar: String::new() }
}

fn reason(e: TextError) -> (SortUnavailable, String) {
    match e {
        TextError::Unavailable(reason, message) => (reason, message),
        TextError::Other(m) => panic!("expected a reason, got {m}"),
    }
}

#[test]
fn statuses_map_to_reasons() {
    for (status, error_type, expected) in [
        (401, None, SortUnavailable::BadApiKey),
        (403, None, SortUnavailable::BadApiKey),
        (429, None, SortUnavailable::RateLimited),
        (500, None, SortUnavailable::ServiceError),
        (529, Some("overloaded_error"), SortUnavailable::ServiceError),
        (400, Some("invalid_request_error"), SortUnavailable::RequestRejected),
        (402, Some("billing_error"), SortUnavailable::RequestRejected),
        (403, Some("billing_error"), SortUnavailable::BadApiKey),
        (418, Some("billing_error"), SortUnavailable::RequestRejected),
        (404, None, SortUnavailable::ServiceError),
    ] {
        assert_eq!(map_status(status, error_type), expected, "{status} {error_type:?}");
    }
}

#[test]
fn transport_failures_are_no_network_or_timed_out() {
    for (text, expected) in [
        ("Dns Failed: resolve dns name 'api.anthropic.com:443'", SortUnavailable::NoNetwork),
        ("Connection Failed: Connect error: connection refused", SortUnavailable::NoNetwork),
        ("Connection Failed: Connect error: connection timed out", SortUnavailable::NoNetwork),
        ("Network Error: timed out reading response", SortUnavailable::TimedOut),
    ] {
        assert_eq!(reason(transport_error(text, SECRET)).0, expected, "{text}");
    }
    assert!(!reason(transport_error(&format!("proxy said {SECRET}"), SECRET)).1.contains(SECRET));
}

#[test]
fn describe_covers_every_reason() {
    for (reason, sentence) in [
        (SortUnavailable::NoApiKey, "Add a Claude API key in Settings to categorize"),
        (SortUnavailable::NoNetwork, "No connection; categorizing will try again after the next run"),
        (SortUnavailable::BadApiKey, "Claude didn't accept the API key; check it in Settings"),
        (SortUnavailable::RateLimited, "Claude is busy; categorizing will try again after the next run"),
        (SortUnavailable::ServiceError, "Claude had a problem; categorizing will try again after the next run"),
        (SortUnavailable::TimedOut, "Claude took too long; categorizing will try again after the next run"),
        (SortUnavailable::RequestRejected, "Claude rejected the request; check your Claude account, for example its credit balance"),
        (SortUnavailable::Refused, "Claude declined to categorize some items; they stay uncategorized"),
        (SortUnavailable::ModelNotDownloaded, "Download the sorting model in Settings"),
        (SortUnavailable::ModelLoadFailed, "The sorting model couldn't start"),
    ] {
        assert_eq!(reason.describe(), sentence, "{reason:?}");
    }
}

#[test]
fn no_key_is_no_api_key_without_a_request() {
    for key in [None, Some("   ".to_string())] {
        let model = ClaudeTextModel::new(move || key.clone());
        assert_eq!(reason(model.complete("sys", "user", &format(), 900).unwrap_err()).0, SortUnavailable::NoApiKey);
    }
}

/// A cancel that lands before the request exists skips it.
#[test]
fn cancel_before_the_call_starts_skips_it() {
    let model = ClaudeTextModel::new(|| Some(SECRET.to_string()));
    model.load().unwrap();
    model.cancel();
    let reply = model.complete("sys", "user", &format(), 900).unwrap();
    assert_eq!((reply.stop_reason.as_str(), reply.text.as_str()), ("user", ""));
}

#[test]
fn a_refused_call_logs_status_and_server_error_but_never_the_key() {
    let body = json!({"type": "error", "error": {"type": "invalid_request_error", "message": format!("Your credit balance is too low\n (key {SECRET})")}});
    let (why, message) = reason(status_error(400, &body, SECRET));
    assert_eq!(why, SortUnavailable::RequestRejected);
    for part in ["400", "invalid_request_error", "Your credit balance is too low (key [key])"] {
        assert!(message.contains(part), "{message}");
    }
    assert!(!message.contains(SECRET));
    // A long server message is cut, and a body that isn't the API's still gives a reason.
    let long = json!({"error": {"type": "api_error", "message": "x".repeat(5000)}});
    assert!(reason(status_error(500, &long, SECRET)).1.len() < 400);
    assert_eq!(reason(status_error(401, &Value::Null, SECRET)).0, SortUnavailable::BadApiKey);
}

#[test]
fn request_has_an_explicit_timeout_of_five_minutes() {
    assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(300));
}

#[test]
fn refusal_is_its_own_reason() {
    let body = json!({"stop_reason": "refusal", "stop_details": {"category": "cyber"}, "content": []});
    let (why, message) = reason(read_reply(&body, 5).unwrap_err());
    assert_eq!(why, SortUnavailable::Refused);
    assert!(message.contains("cyber"));
    assert_eq!(reason(read_reply(&json!({"stop_reason": "refusal"}), 5).unwrap_err()).0, SortUnavailable::Refused);
}

#[test]
fn success_returns_text_and_usage() {
    let body = json!({
        "stop_reason": "end_turn",
        "content": [{"type": "thinking", "thinking": "hm"}, {"type": "text", "text": "{\"new_categories\":[],"}, {"type": "text", "text": "\"assignments\":[]}"}],
        "usage": {"input_tokens": 1234, "output_tokens": 56}
    });
    let reply = read_reply(&body, 77).unwrap();
    assert_eq!(reply.text, r#"{"new_categories":[],"assignments":[]}"#);
    assert_eq!((reply.stop_reason.as_str(), reply.input_tokens, reply.output_tokens, reply.millis), ("end_turn", 1234, 56, 77));
}

#[test]
fn request_follows_the_global_constraints() {
    let body = request_body("the system", "the user", &format());
    assert_eq!(body["model"], json!("claude-sonnet-5-5"));
    assert_eq!(body["max_tokens"], json!(16000));
    for absent in ["thinking", "temperature", "top_p", "top_k"] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    assert_eq!(body["output_config"]["effort"], json!("medium"));
    assert_eq!(body["output_config"]["format"]["type"], json!("json_schema"));
    let schema = body["output_config"]["format"]["schema"].to_string();
    assert!(schema.contains("\"assignments\"") && !schema.contains("maxItems"), "{schema}");
    assert_eq!(body["fallbacks"], json!("default"));
    assert_eq!(FALLBACK_BETA, "server-side-fallback-2026-07-01");
    assert_eq!(body["system"], json!("the system"));
    assert_eq!(body["messages"], json!([{"role": "user", "content": "the user"}]));
}

#[test]
fn wire_schema_drops_unsupported_keywords_but_keeps_properties_named_like_them() {
    let schema = json!({
        "type": "object",
        "properties": {
            "list": {"type": "array", "items": {"type": "string", "maxLength": 4}, "maxItems": 12, "minItems": 1},
            "maxItems": {"type": "integer", "minimum": 0},
            "many": {"type": "array", "items": {"type": "string"}, "minItems": 3},
        },
        "required": ["list", "maxItems", "many"],
        "additionalProperties": false,
    });
    let expected = json!({
        "type": "object",
        "properties": {
            "list": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "maxItems": {"type": "integer"},
            "many": {"type": "array", "items": {"type": "string"}},
        },
        "required": ["list", "maxItems", "many"],
        "additionalProperties": false,
    });
    assert_eq!(wire_schema(&schema), expected);
}
