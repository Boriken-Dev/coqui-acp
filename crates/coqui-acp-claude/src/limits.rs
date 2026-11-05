//! Usage, quota and usage limits as Claude Code reports them.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use coqui_acp_harness::event::{Limit, Quota, Usage};
use serde_json::Value;

/// A Unix timestamp that may be in seconds or milliseconds, in seconds.
pub(crate) const fn unix_seconds(value: i64) -> i64 {
    if value > 100_000_000_000 {
        value / 1000
    } else {
        value
    }
}

/// Claude Code's refusal to go on until a quota resets: a `rate_limit_event` whose status is `rejected`, or an error
/// `result` whose text names a limit (`usage limit`, `limit reached`, `hit your limit`) or whose `api_error_status`
/// is 429. A reset time comes from `resetsAt`, or from the number after a trailing `|` of the text; one given only
/// as a local clock time is left unset rather than guessed.
pub(crate) fn limit(value: &Value) -> Option<Limit> {
    match value.get("type").and_then(Value::as_str)? {
        "rate_limit_event" => {
            let info = value.get("rate_limit_info")?;
            if info.get("status").and_then(Value::as_str) != Some("rejected") {
                return None;
            }
            let mut limit = Limit::default();
            limit.resets_at = info
                .get("resetsAt")
                .and_then(Value::as_i64)
                .map(unix_seconds);
            limit.window = info
                .get("rateLimitType")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(limit)
        }
        "result" if value.get("is_error").and_then(Value::as_bool) == Some(true) => {
            let text = value.get("result").and_then(Value::as_str)?;
            let lower = text.to_lowercase();
            let limited = value.get("api_error_status").and_then(Value::as_i64) == Some(429)
                || ["usage limit", "limit reached", "hit your limit"]
                    .iter()
                    .any(|phrase| lower.contains(phrase));
            if !limited {
                return None;
            }
            let resets_at = text
                .rsplit_once('|')
                .and_then(|(_, tail)| tail.trim().parse::<i64>().ok())
                .map(unix_seconds);
            let window = if lower.contains("5-hour") || lower.contains("five-hour") {
                Some("five_hour".to_string())
            } else if lower.contains("weekly") || lower.contains("7-day") {
                Some("weekly".to_string())
            } else {
                None
            };
            let mut limit = Limit::default();
            limit.resets_at = resets_at;
            limit.window = window;
            limit.text = Some(text.to_string());
            Some(limit)
        }
        _ => None,
    }
}

/// The quota window a `rate_limit_event` reports, whatever its status: `utilization` as thousandths only when it is a
/// fraction between 0 and 1, since its scale is not documented.
pub(crate) fn quota(value: &Value) -> Vec<Quota> {
    if value.get("type").and_then(Value::as_str) != Some("rate_limit_event") {
        return Vec::new();
    }
    let Some(info) = value.get("rate_limit_info") else {
        return Vec::new();
    };
    let mut quota = Quota::new(
        info.get("rateLimitType")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
    );
    quota.used_permille = info
        .get("utilization")
        .and_then(Value::as_f64)
        .filter(|fraction| (0.0..=1.0).contains(fraction))
        .map(|fraction| permille(fraction * 1000.0));
    quota.resets_at = info
        .get("resetsAt")
        .and_then(Value::as_i64)
        .map(unix_seconds);
    quota.status = info
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    vec![quota]
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=1000 first"
)]
fn permille(value: f64) -> u16 {
    value.round().clamp(0.0, 1000.0) as u16
}

/// The usage a `result` reports: its `usage` token counts and `total_cost_usd`, running totals for the process, and
/// the model's context size from `modelUsage`. `context_used` is the caller's, from the last model request.
pub(crate) fn usage(value: &Value) -> Option<Usage> {
    if value.get("type").and_then(Value::as_str)? != "result" {
        return None;
    }
    let counts = value.get("usage");
    let cost = value.get("total_cost_usd").and_then(Value::as_f64);
    if counts.is_none() && cost.is_none() {
        return None;
    }
    let count = |key: &str| {
        counts
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let size = value
        .get("modelUsage")
        .and_then(Value::as_object)
        .and_then(|models| {
            models
                .values()
                .filter_map(|model| model.get("contextWindow").and_then(Value::as_u64))
                .max()
        });
    let mut usage = Usage::default();
    usage.input = count("input_tokens");
    usage.output = count("output_tokens");
    usage.cache_read = count("cache_read_input_tokens");
    usage.cache_write = count("cache_creation_input_tokens");
    usage.cumulative = true;
    usage.cost_usd = cost;
    usage.context_size = size;
    Some(usage)
}

/// The tokens of the model's context one request used: its input, cached or not, and its output.
pub(crate) fn context_used(message_usage: &Value) -> Option<u64> {
    let count = |key: &str| message_usage.get(key).and_then(Value::as_u64);
    let input = count("input_tokens")?;
    Some(
        input
            + count("cache_read_input_tokens").unwrap_or(0)
            + count("cache_creation_input_tokens").unwrap_or(0)
            + count("output_tokens").unwrap_or(0),
    )
}

/// Whether a `result` says the turn did not do its work: `is_error`, which Claude Code sets with `subtype: success`
/// when no login reached a model, or an `error_*` subtype.
pub(crate) fn is_error(value: &Value) -> bool {
    value.get("is_error").and_then(Value::as_bool) == Some(true)
        || value
            .get("subtype")
            .and_then(Value::as_str)
            .is_some_and(|subtype| subtype.starts_with("error"))
}

/// What a failed `result` says: its `result` text, else its `errors`, one per line.
pub(crate) fn error_text(value: &Value) -> String {
    if let Some(text) = value.get("result").and_then(Value::as_str) {
        return text.to_string();
    }
    let errors: Vec<&str> = value
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if errors.is_empty() {
        value
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("the turn failed")
            .to_string()
    } else {
        errors.join("\n")
    }
}

/// The tools a `result` lists under `permission_denials`, each named once, in the order first refused. A call's
/// input stays out, since it may hold anything the model wrote.
pub(crate) fn denials(value: &Value) -> Vec<String> {
    let mut tools: Vec<String> = Vec::new();
    for denial in value
        .get("permission_denials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(tool) = denial.get("tool_name").and_then(Value::as_str)
            && !tools.iter().any(|known| known == tool)
        {
            tools.push(tool.to_string());
        }
    }
    tools
}
