//! Antigravity's `GetUserStatus` answer, read into the few fields a bucket needs, and the buckets
//! those make.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use coqui_acp_api::usage::{Bucket, Coverage, Refill, Status, Unit};
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{AcpModel, ReadError};

/// What the permille limit of a bucket is.
const WHOLE: u64 = 1000;

/// The answer, holding only the model list. Every other field (the account's address, name and
/// picture, its tier) is skipped as it is read and never becomes a value.
#[derive(Deserialize)]
struct Answer {
    #[serde(rename = "userStatus")]
    status: Option<UserStatus>,
}

#[derive(Deserialize)]
struct UserStatus {
    #[serde(rename = "cascadeModelConfigData")]
    models: Option<ModelData>,
}

#[derive(Deserialize)]
struct ModelData {
    #[serde(rename = "clientModelConfigs", default)]
    configs: Vec<Config>,
}

#[derive(Deserialize)]
struct Config {
    label: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    #[serde(rename = "quotaInfo")]
    quota: Option<Quota>,
}

/// The two figures stay loose values so that one model with a figure of the wrong type is skipped
/// rather than failing the whole answer.
#[derive(Deserialize)]
struct Quota {
    #[serde(rename = "remainingFraction")]
    fraction: Option<Value>,
    #[serde(rename = "resetTime")]
    reset: Option<Value>,
}

/// One model's allowance as agy reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Model {
    /// agy's id for the model, the ACP model id when it sends one.
    pub(super) id: Option<String>,
    pub(super) label: Option<String>,
    /// What is left, in thousandths.
    pub(super) permille: u64,
    /// When the window ends, in Unix seconds.
    pub(super) resets_at: i64,
}

/// Reads `body` as agy's status.
///
/// A model without a usable allowance, a fraction that is not a number in `0..=1` or a reset that is
/// not an RFC 3339 time, or one that names itself by neither id nor label, is left out.
pub(super) fn parse(body: &[u8]) -> Result<Vec<Model>, ReadError> {
    let answer: Answer = serde_json::from_slice(body).map_err(|_| ReadError::Malformed)?;
    let data = answer
        .status
        .and_then(|status| status.models)
        .ok_or(ReadError::Malformed)?;
    Ok(data.configs.into_iter().filter_map(model).collect())
}

fn model(config: Config) -> Option<Model> {
    let quota = config.quota?;
    let fraction = quota.fraction?.as_f64()?;
    if !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    let resets_at = OffsetDateTime::parse(quota.reset?.as_str()?, &Rfc3339)
        .ok()?
        .unix_timestamp();
    let id = config.model_id.filter(|id| !id.is_empty());
    let label = config.label.filter(|label| !label.is_empty());
    if id.is_none() && label.is_none() {
        return None;
    }
    Some(Model {
        id,
        label,
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the fraction is within 0..=1"
        )]
        permille: (fraction * 1000.0).round() as u64,
        resets_at,
    })
}

/// What a reader remembers between readings: how long each bucket's window is.
#[derive(Debug, Default)]
pub(super) struct Windows(BTreeMap<String, Duration>);

/// The buckets of `models` as of `now` (Unix seconds): one for each distinct reset time, in the
/// order the first of its models appears. Models agy lists that `acp` does not (by id, then by
/// label) are counted in `unmatched`.
pub(super) fn buckets(
    models: &[Model],
    acp: &[AcpModel],
    now: i64,
    windows: &mut Windows,
    unmatched: &mut u64,
) -> Vec<Bucket> {
    let mut groups: Vec<(i64, Vec<&Model>)> = Vec::new();
    for model in models {
        match groups.iter_mut().find(|(at, _)| *at == model.resets_at) {
            Some((_, members)) => members.push(model),
            None => groups.push((model.resets_at, alloc::vec![model])),
        }
    }
    groups
        .into_iter()
        .map(|(resets_at, members)| bucket(resets_at, &members, acp, now, windows, unmatched))
        .collect()
}

fn bucket(
    resets_at: i64,
    members: &[&Model],
    acp: &[AcpModel],
    now: i64,
    windows: &mut Windows,
    unmatched: &mut u64,
) -> Bucket {
    let first = members[0];
    let name = first.id.as_deref().or(first.label.as_deref()).unwrap_or("");
    let mut bucket = Bucket::new(alloc::format!("agy:{name}"));

    let mut covered: Vec<String> = Vec::new();
    let mut all_matched = true;
    for member in members {
        match find(member, acp) {
            Some(id) => {
                if !covered.iter().any(|held| held == id) {
                    covered.push(id.into());
                }
            }
            None => {
                all_matched = false;
                *unmatched += 1;
            }
        }
    }
    if all_matched {
        bucket.models = Coverage::Only { models: covered };
    }

    let remaining = members.iter().map(|m| m.permille).min().unwrap_or(0);
    bucket.limit = Some(WHOLE);
    bucket.remaining = Some(remaining);
    bucket.status = if remaining == 0 {
        Status::Exhausted
    } else {
        Status::Ok
    };
    bucket.unit = Unit::Permille;
    bucket.observed_at = now;

    // An unused allowance has no window yet: agy reports a reset a whole window away that moves with
    // every reading, so it is a length to remember and not a time to keep.
    let length = if remaining == WHOLE {
        let length = window_length(resets_at, now);
        if let Some(length) = length {
            windows.0.insert(bucket.id.clone(), length);
        }
        length
    } else {
        windows.0.get(&bucket.id).copied()
    };
    bucket.refill = Refill::FixedWindow {
        length,
        resets_at: (remaining != WHOLE).then_some(resets_at),
    };
    bucket.label = length.map(name_of);
    bucket
}

/// The ACP id of the model `agy` describes: by id, else by label against the ACP model's name.
fn find<'a>(model: &Model, acp: &'a [AcpModel]) -> Option<&'a str> {
    let by_id = model
        .id
        .as_deref()
        .and_then(|id| acp.iter().find(|held| held.id == id));
    by_id
        .or_else(|| {
            let label = model.label.as_deref()?;
            acp.iter().find(|held| held.name == label)
        })
        .map(|held| held.id.as_str())
}

/// Time until `resets_at`, to the nearest minute.
fn window_length(resets_at: i64, now: i64) -> Option<Duration> {
    let seconds = u64::try_from(resets_at.checked_sub(now)?).ok()?;
    let minutes = seconds.checked_add(30)? / 60;
    (minutes > 0).then(|| Duration::from_secs(minutes * 60))
}

/// A window as a person reads it: `5 h`, `7 d`.
fn name_of(length: Duration) -> String {
    let minutes = length.as_secs() / 60;
    if minutes.checked_rem(24 * 60) == Some(0) {
        alloc::format!("{} d", minutes / (24 * 60))
    } else if minutes.checked_rem(60) == Some(0) {
        alloc::format!("{} h", minutes / 60)
    } else {
        alloc::format!("{minutes} min")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_at(id: &str, permille: u64, resets_at: i64) -> Model {
        Model {
            id: Some(id.into()),
            label: None,
            permille,
            resets_at,
        }
    }

    #[test]
    fn a_window_is_named_as_a_person_reads_it() {
        assert_eq!(name_of(Duration::from_secs(18_000)), "5 h");
        assert_eq!(name_of(Duration::from_secs(604_800)), "7 d");
        assert_eq!(name_of(Duration::from_secs(90 * 60)), "90 min");
    }

    #[test]
    fn a_length_is_rounded_to_the_minute() {
        assert_eq!(
            window_length(18_000 - 1, 0),
            Some(Duration::from_secs(18_000))
        );
        assert_eq!(window_length(10, 0), None);
        assert_eq!(window_length(0, 10), None);
    }

    #[test]
    fn the_least_left_in_a_group_is_the_buckets() {
        let models = [model_at("a", 700, 9_000), model_at("b", 400, 9_000)];
        let mut windows = Windows::default();
        let made = buckets(&models, &[], 0, &mut windows, &mut 0);
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].remaining, Some(400));
        assert_eq!(made[0].id, "agy:a");
    }

    #[test]
    fn the_recorded_answer_makes_two_unused_buckets_of_five_hours_and_seven_days() {
        let body = include_str!("../../tests/fixtures/agy-1.5.3/getuserstatus.json");
        let models = parse(body.as_bytes()).unwrap();
        assert_eq!(models.len(), 13);
        let now = models[0].resets_at - 604_800;
        let mut windows = Windows::default();
        let made = buckets(&models, &[], now, &mut windows, &mut 0);
        let shown: Vec<_> = made
            .iter()
            .map(|b| (b.id.as_str(), b.label.as_deref(), b.remaining))
            .collect();
        assert_eq!(
            shown,
            [
                ("agy:gemini-4-argon-medium", Some("7 d"), Some(1000)),
                ("agy:gemini-3.8-flash-high", Some("5 h"), Some(1000)),
            ]
        );
        assert!(made.iter().all(|b| b.models == Coverage::Unknown));
    }
}
