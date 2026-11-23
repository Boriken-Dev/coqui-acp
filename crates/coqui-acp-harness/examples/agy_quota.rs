//! Reads the quota of the Antigravity account stored on this machine and prints it as `agy`'s `/usage` does: each bucket's group,
//! window, the models it covers, what is left and when it resets. It prints no token, no email and no identifier.
//!
//! `cargo run -p coqui-acp-harness --features antigravity --example agy_quota`

use coqui_acp_api::usage::{Coverage, Refill, Status};
use coqui_acp_harness::antigravity::{AgyLogin, LoginReading, LoginSettings};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn main() {
    let mut login = AgyLogin::new(LoginSettings::default());
    let signed = match login.read(&[]) {
        Ok(LoginReading::SignedIn(signed)) => signed,
        Ok(_) => return println!("not signed in"),
        Err(error) => return println!("login not read: {error}"),
    };
    let buckets = match signed.usage {
        Ok(buckets) => buckets,
        Err(error) => return println!("quota not read: {error}"),
    };
    for bucket in buckets {
        let group = bucket.group.as_deref().unwrap_or("-");
        let label = bucket.label.as_deref().unwrap_or(&bucket.id);
        let models = if bucket.model_names.is_empty() {
            match bucket.models {
                Coverage::Only { ref models } => models.join(", "),
                _ => "all models".to_owned(),
            }
        } else {
            bucket.model_names.join(", ")
        };
        let left = match (&bucket.status, bucket.remaining) {
            (Status::Disabled, _) => "disabled".to_owned(),
            (_, Some(left)) => format!("{}%", (left + 5) / 10),
            _ => "?".to_owned(),
        };
        let resets = match bucket.refill {
            Refill::FixedWindow {
                resets_at: Some(at),
                ..
            } if bucket.status != Status::Disabled => OffsetDateTime::from_unix_timestamp(at)
                .ok()
                .and_then(|time| time.format(&Rfc3339).ok())
                .unwrap_or_default(),
            _ => String::new(),
        };
        println!("{group:<24} {label:<30} {left:<9} {resets:<22} {models}");
    }
}
