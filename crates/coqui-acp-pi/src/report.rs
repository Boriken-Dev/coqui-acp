//! The failure report the gate extension sends: why a model request failed, as the provider said it.
//!
//! Pi's own events show a failed request only as one string. The gate extension (`gate.ts`) also sees the provider's
//! response (status, headers) and the error's type and code, and sends them as a `setStatus` request under
//! [`REPORT_KEY`] whose text is JSON. This reads the limit out of it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

use coqui_acp_api::usage::{LimitHit, LimitKind, Source};
use coqui_acp_harness::lines::cut;
use serde_json::Value;

/// The `setStatus` key of the extension's reports.
pub(crate) const REPORT_KEY: &str = "coqui-acp-pi-report";

/// The report's version this reads.
const VERSION: u64 = 1;

/// The most of the provider's words kept in the limit's text.
const TEXT_BYTES: usize = 512;

/// The limit a report says, when it says one. `observed_at` is 0: the connection stamps it.
pub(crate) fn limit_of(text: &str) -> Option<LimitHit> {
    let report: Value = serde_json::from_str(text).ok()?;
    if report.get("v").and_then(Value::as_u64) != Some(VERSION) {
        return None;
    }
    let limit = report.get("limit").filter(|limit| limit.is_object())?;
    let kind = limit
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())?;
    let mut hit = LimitHit::new(0).kind(LimitKind::from(kind.to_string()));
    hit.source = Source::Reported;
    hit.bucket = limit
        .get("bucket")
        .and_then(Value::as_str)
        .filter(|bucket| !bucket.is_empty())
        .map(String::from);
    hit.resets_at = limit.get("resets_at").and_then(Value::as_i64);
    hit.retry_after = limit
        .get("retry_after_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis);
    hit.continuable = limit.get("continuable").and_then(Value::as_bool);
    hit.text = report
        .pointer("/error/message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(|message| cut(message, TEXT_BYTES).to_string());
    hit.text_ids = Vec::new();
    Some(hit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GATE_SOURCE;

    #[test]
    fn the_extension_sends_under_the_key_this_reads() {
        assert!(GATE_SOURCE.contains(&alloc::format!("\"{REPORT_KEY}\"")));
    }

    #[test]
    fn a_limit_is_read_field_by_field() {
        let hit = limit_of(
            r#"{"v":1,"error":{"message":"slow down"},"limit":{"kind":"usage_quota","bucket":"five_hour","resets_at":1800003600,"retry_after_ms":1500,"continuable":false}}"#,
        )
        .expect("a limit");
        assert_eq!(hit.kind, Some(LimitKind::UsageQuota));
        assert_eq!(hit.bucket.as_deref(), Some("five_hour"));
        assert_eq!(hit.resets_at, Some(1_800_003_600));
        assert_eq!(hit.retry_after, Some(Duration::from_millis(1500)));
        assert_eq!(hit.continuable, Some(false));
        assert_eq!(hit.text.as_deref(), Some("slow down"));
        assert_eq!(hit.source, Source::Reported);
        assert_eq!(hit.observed_at, 0);
    }

    #[test]
    fn what_is_not_a_limit_report_is_none() {
        for text in [
            "not json",
            r#"{"v":2,"limit":{"kind":"rate_limit"}}"#,
            r#"{"v":1,"error":{"status":500}}"#,
            r#"{"v":1,"limit":{"kind":""}}"#,
            r#"{"v":1,"limit":"rate_limit"}"#,
        ] {
            assert!(limit_of(text).is_none(), "{text}");
        }
    }

    #[test]
    fn a_kind_a_newer_extension_names_is_kept() {
        let hit = limit_of(r#"{"v":1,"limit":{"kind":"seat_cap"}}"#).expect("a limit");
        assert_eq!(hit.kind, Some(LimitKind::Other("seat_cap".to_string())));
    }
}
