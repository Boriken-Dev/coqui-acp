//! Usage, quota and usage limits as Codex's app server reports them.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use coqui_acp_harness::event::{Limit, Quota, Usage};
use serde_json::Value;

/// The name of a quota window by its length, else its position.
fn window_name(window: &Value, position: &str) -> String {
    match window.get("windowDurationMins").and_then(Value::as_i64) {
        Some(300) => "five_hour".to_string(),
        Some(10_080) => "weekly".to_string(),
        _ => position.to_string(),
    }
}

/// `percent` as thousandths, clamped to 0..=1000 before the cast.
fn permille(percent: f64) -> u16 {
    (percent * 10.0).round().clamp(0.0, 1000.0) as u16
}

/// Every quota window an `account/rateLimits/updated` notification reports: `usedPercent` as thousandths,
/// `resetsAt` in Unix seconds.
pub(crate) fn quota(message: &Value) -> Vec<Quota> {
    if message.get("method").and_then(Value::as_str) != Some("account/rateLimits/updated") {
        return Vec::new();
    }
    let Some(limits) = message.pointer("/params/rateLimits") else {
        return Vec::new();
    };
    ["primary", "secondary"]
        .iter()
        .filter_map(|position| {
            let window = limits.get(position).filter(|window| window.is_object())?;
            let mut quota = Quota::new(window_name(window, position));
            quota.used_permille = window
                .get("usedPercent")
                .and_then(Value::as_f64)
                .map(permille);
            quota.resets_at = window.get("resetsAt").and_then(Value::as_i64);
            Some(quota)
        })
        .collect()
}

/// The window an `account/rateLimits/updated` notification reports as used up, the one that resets last: the
/// usage-limit error that follows does not always repeat the reset time.
pub(crate) fn exhausted(message: &Value) -> Option<Limit> {
    if message.get("method")?.as_str()? != "account/rateLimits/updated" {
        return None;
    }
    let limits = message.pointer("/params/rateLimits")?;
    ["primary", "secondary"]
        .iter()
        .filter_map(|key| {
            let window = limits.get(key)?;
            (window.get("usedPercent")?.as_f64()? >= 100.0).then(|| {
                let mut limit = Limit::default();
                limit.resets_at = window.get("resetsAt").and_then(Value::as_i64);
                limit.window = match window.get("windowDurationMins").and_then(Value::as_i64) {
                    Some(300 | 10_080) => Some(window_name(window, "")),
                    _ => None,
                };
                limit
            })
        })
        .max_by_key(|limit| limit.resets_at)
}

/// The error object of an `error` or `turn/failed` notification, or of a failed `turn/completed`.
pub(crate) fn error_of(message: &Value) -> Option<&Value> {
    let params = message.get("params")?;
    let error = match message.get("method")?.as_str()? {
        "error" => params.get("error"),
        "turn/failed" => params
            .get("error")
            .or_else(|| params.pointer("/turn/error")),
        "turn/completed"
            if params.pointer("/turn/status").and_then(Value::as_str) == Some("failed") =>
        {
            params.pointer("/turn/error")
        }
        _ => None,
    };
    error.filter(|error| !error.is_null())
}

/// A usage limit in an error: `codexErrorInfo` naming a usage or rate limit, or a connection that failed with HTTP
/// 429. The reset is the error's `resetsAt`, else the exhausted window's `hint`.
pub(crate) fn limit(error: &Value, hint: Option<&Limit>) -> Option<Limit> {
    let info = error.get("codexErrorInfo")?;
    let named = info
        .as_str()
        .map(|text| text.to_lowercase().replace('_', ""))
        .is_some_and(|text| text.contains("usagelimit") || text.contains("ratelimit"));
    let throttled = info
        .as_object()
        .and_then(|object| object.values().next())
        .and_then(|detail| detail.get("httpStatusCode"))
        .and_then(Value::as_u64)
        == Some(429);
    if !named && !throttled {
        return None;
    }
    let mut limit = Limit::default();
    limit.resets_at = error
        .get("resetsAt")
        .and_then(Value::as_i64)
        .or_else(|| hint.and_then(|hint| hint.resets_at));
    limit.window = hint.and_then(|hint| hint.window.clone());
    limit.text = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(limit)
}

/// The tokens one model request used, from `thread/tokenUsage/updated`: its `last`, since the thread's total
/// survives a resume. `inputTokens` include the cached ones, which are split out. Codex reports no cost. The context
/// in use is that request's input and output, of `modelContextWindow`.
pub(crate) fn usage(message: &Value) -> Option<Usage> {
    if message.get("method")?.as_str()? != "thread/tokenUsage/updated" {
        return None;
    }
    let usage = message.pointer("/params/tokenUsage")?;
    let last = usage.get("last")?;
    let count = |key: &str| last.get(key).and_then(Value::as_u64).unwrap_or(0);
    let cached = count("cachedInputTokens");
    let mut reported = Usage::default();
    reported.input = count("inputTokens").saturating_sub(cached);
    reported.output = count("outputTokens");
    reported.cache_read = cached;
    reported.cache_write = count("cacheWriteInputTokens");
    reported.context_used = Some(count("inputTokens").saturating_add(count("outputTokens")));
    reported.context_size = usage.get("modelContextWindow").and_then(Value::as_u64);
    Some(reported)
}
