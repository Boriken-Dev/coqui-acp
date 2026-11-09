//! How much of each usage allowance an account has used, how it refills and when: one model for a
//! harness's or provider's answer, the ACP extension that carries it, and the `acp_harness` kind's
//! `usage` method.
//!
//! The model is plain data and the same in every language: counts and money are integers, times are
//! Unix seconds, lengths are Guatiao's `Duration` (`[seconds, nanoseconds]`). No float appears. It
//! crosses the kind as a Guatiao value and the ACP extension as JSON; both spell it the same, and
//! `vectors/usage.json` holds every reader to that spelling.
//!
//! Warning thresholds are the host's: a provider reports [`Status::Warning`] only when the harness
//! said so, and never invents a level of its own.

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use guatiao::{FromValue, Schema, Secret, ToValue};
use serde::{Deserialize, Serialize};

use crate::environment::Environment;

/// The key of `agentCapabilities._meta` (`capabilities._meta` in ACP version 2) in which an agent
/// that reports usage says so: `{"version": 1, "sources": ["reported", "inferred"], "spend": true}`.
/// Absent means the agent reports none, and a client does not call [`USAGE_METHOD`].
pub const USAGE_CAPABILITY: &str = "dev.boriken.coqui.acp/usage";

/// The request a client sends to read an agent's [`UsageReport`]: `{"sessionId"?: uuid}`. It needs
/// no session and no turn, and is cheap, so a host may poll it as often as it likes. An agent that
/// does not know it answers `-32601`.
pub const USAGE_METHOD: &str = "_coqui_acp/usage";

/// The notification an agent sends, only to a client that asked for it, when its report changed:
/// `{"sessionId"?: uuid, "report": UsageReport}`, the whole report every time.
pub const USAGE_NOTIFICATION: &str = "_coqui_acp/usage_update";

/// The `_meta` key of a turn's answer that says a quota refused the turn. Its value is a
/// [`LimitHit`].
pub const LIMIT_META_KEY: &str = "dev.boriken.coqui.acp/limit";

/// The version of the extension. It is independent of ACP's, and is 1 until the first release.
pub const USAGE_VERSION: u32 = 1;

/// Writes the names of an open enum: a unit enum with one `Other(String)` that keeps a name no
/// variant has. Text equal to a known name always reads as that variant.
macro_rules! open_names {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            /// Its name on the wire.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $name,)+
                    Self::Other(text) => text,
                }
            }
        }

        impl From<String> for $ty {
            fn from(text: String) -> Self {
                match text.as_str() {
                    $($name => Self::$variant,)+
                    _ => Self::Other(text),
                }
            }
        }

        impl From<$ty> for String {
            fn from(value: $ty) -> String {
                match value {
                    $($ty::$variant => $name.into(),)+
                    $ty::Other(text) => text,
                }
            }
        }
    };
}

/// A tagged enum read leniently where it sits in a struct: a `kind` no variant has reads as the
/// type's `Other`, never as a failure, so a newer writer's value does not lose the whole report.
macro_rules! lenient {
    ($module:ident, $ty:ident, [$($known:literal),+ $(,)?]) => {
        mod $module {
            use guatiao::schema::KindBuilder;
            use guatiao::{Alloc, FromValue, Map, MapError, Schema, ToValue, Value, ValueError};

            use super::$ty;

            pub(super) fn to_value_in(value: &$ty, alloc: Alloc) -> Result<Value, ValueError> {
                value.to_value_in(alloc)
            }

            pub(super) fn from_value(value: &Value) -> Result<$ty, MapError> {
                let named = <&Map>::try_from(value)
                    .ok()
                    .and_then(|map| map.get("kind"))
                    .and_then(|kind| <&str>::try_from(kind).ok());
                match named {
                    Some(kind) if ![$($known),+].contains(&kind) => Ok($ty::Other),
                    _ => $ty::from_value(value),
                }
            }

            pub(super) fn kind(alloc: Alloc) -> KindBuilder {
                <$ty as Schema>::kind(alloc)
            }
        }
    };
}

/// Where a figure came from.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, rename_all = "snake_case")]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum Source {
    /// The harness's own structured figure.
    Reported,
    /// Read from an error's words, a 429 or a refusal, by the provider or the host. The bucket's
    /// `evidence` holds the words, for logs; nothing parses it.
    Inferred,
    /// A source a newer writer named.
    #[map(other)]
    Other(String),
}

open_names!(Source { Reported => "reported", Inferred => "inferred" });

/// What a bucket counts.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, rename_all = "snake_case")]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum Unit {
    /// Tokens.
    Tokens,
    /// Requests.
    Requests,
    /// Millionths of a US dollar.
    MicroUsd,
    /// A percentage only, when the provider gives no absolute number: `limit` is 1000 and `used`
    /// thousandths, so the arithmetic is the same for every unit.
    Permille,
    /// A unit a newer writer named. A reader treats the bucket as unknown, and never fails.
    #[map(other)]
    Other(String),
}

open_names!(Unit { Tokens => "tokens", Requests => "requests", MicroUsd => "micro_usd", Permille => "permille" });

/// What a bucket's account may do now.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, rename_all = "snake_case")]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum Status {
    /// Room is left.
    Ok,
    /// The harness said it is nearly used. Only when the harness said so: a host applies its own
    /// thresholds to the bucket's fraction.
    Warning,
    /// Nothing is left until the bucket refills.
    Exhausted,
    /// The harness did not say.
    Unknown,
    /// A status a newer writer named.
    #[map(other)]
    Other(String),
}

open_names!(Status { Ok => "ok", Warning => "warning", Exhausted => "exhausted", Unknown => "unknown" });

/// The models a bucket covers.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, tag = "kind", rename_all = "snake_case")]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Coverage {
    /// Every model of the account.
    All,
    /// The harness did not say. A reader counts it as covering every model, so an exhausted bucket
    /// of unknown coverage stops the account for each of them.
    Unknown,
    /// These models only, by the ids or aliases `models()` lists.
    Only {
        /// The ids.
        models: Vec<String>,
    },
    /// A coverage a newer writer named; a reader treats it as unknown.
    #[serde(other)]
    Other,
}

lenient!(
    coverage_codec,
    Coverage,
    ["all", "unknown", "only", "other"]
);

/// How a bucket refills.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, tag = "kind", rename_all = "snake_case")]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Refill {
    /// All at once when the window ends, such as a five-hour or weekly allowance.
    FixedWindow {
        /// How long the window is, when the harness says.
        #[serde(default, skip_serializing_if = "Option::is_none", with = "pair_option")]
        length: Option<Duration>,
        /// When it ends, in Unix seconds, when the harness says.
        #[serde(skip_serializing_if = "Option::is_none")]
        resets_at: Option<i64>,
    },
    /// Continuously: `amount` units each `per`, up to `max`.
    TokenBucket {
        /// The units added each `per`.
        amount: u64,
        /// The period.
        #[serde(with = "pair")]
        per: Duration,
        /// The most it holds.
        max: u64,
    },
    /// A balance that never refills.
    #[map(rename = "none")]
    #[serde(rename = "none")]
    Never,
    /// The harness did not say.
    Unknown,
    /// A refill a newer writer named; a reader treats it as unknown.
    #[serde(other)]
    Other,
}

lenient!(
    refill_codec,
    Refill,
    ["fixed_window", "token_bucket", "none", "unknown", "other"]
);

// The serde spelling of a length, `[seconds, nanoseconds]`, is Guatiao's.
mod pair {
    use core::time::Duration;

    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(value: &Duration, to: S) -> Result<S::Ok, S::Error> {
        (value.as_secs(), value.subsec_nanos()).serialize(to)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Duration, D::Error> {
        let (secs, nanos) = <(u64, u32)>::deserialize(from)?;
        if nanos >= 1_000_000_000 {
            return Err(D::Error::custom("nanoseconds below 1000000000"));
        }
        Ok(Duration::new(secs, nanos))
    }
}

mod pair_option {
    use core::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Pair(#[serde(with = "super::pair")] Duration);

    #[allow(
        clippy::ref_option,
        reason = "serde's `with` passes the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(
        value: &Option<Duration>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(Pair).serialize(to)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<Pair>::deserialize(from)?.map(|pair| pair.0))
    }
}

/// One allowance: what it covers, how much of it is used and how it refills.
///
/// A provider fills only what it was told and never derives one figure from another; the fraction
/// a reader uses is [`UsageReport::fraction_permille`].
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct Bucket {
    /// Stable per harness and account: `five_hour`, `seven_day`, `seven_day_opus`, `codex:primary`.
    pub id: String,
    /// What a person sees, when the harness has a name apart from the id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The models it covers.
    #[map(with = coverage_codec)]
    pub models: Coverage,
    /// What it counts.
    pub unit: Unit,
    /// How much is used, when the harness said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    /// How much the bucket holds, when the harness said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// How much is left, when the harness said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    /// How it refills.
    #[map(with = refill_codec)]
    pub refill: Refill,
    /// What the harness says of it.
    pub status: Status,
    /// When these figures were read, in Unix seconds.
    pub observed_at: i64,
    /// Where the figures came from.
    pub source: Source,
    /// The words an inferred bucket was read from, for logs; nothing parses them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl Bucket {
    /// A bucket named `id` with nothing known of it: unknown coverage, refill and status, in
    /// permille, reported.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: None,
            models: Coverage::Unknown,
            unit: Unit::Permille,
            used: None,
            limit: None,
            remaining: None,
            refill: Refill::Unknown,
            status: Status::Unknown,
            observed_at: 0,
            source: Source::Reported,
            evidence: None,
        }
    }

    /// How long until a fixed window ends, as of `now` in Unix seconds: zero once its end has
    /// passed (the bucket is then due a new report); `None` for a bucket that is not a fixed
    /// window or does not say when it ends.
    #[must_use]
    pub fn resets_in(&self, now: i64) -> Option<Duration> {
        match &self.refill {
            Refill::FixedWindow {
                resets_at: Some(at),
                ..
            } => Some(Duration::from_secs(
                u64::try_from(at.saturating_sub(now)).unwrap_or(0),
            )),
            _ => None,
        }
    }
}

/// A refused request: the harness said a limit stopped it.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct LimitHit {
    /// The bucket it names, which is `exhausted` in the same report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    /// When the limit lifts, in Unix seconds, when the harness said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// How long to wait before trying again, when the refusal said (a 429's `retry-after`). It may
    /// move, which is why a host reads the report again rather than trusting it once.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "pair_option")]
    pub retry_after: Option<Duration>,
    /// Whether the harness said it or the words were read.
    pub source: Source,
    /// The harness's words, for people.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// When it was seen, in Unix seconds.
    pub observed_at: i64,
}

impl LimitHit {
    /// A reported limit seen at `observed_at`, with nothing else known.
    #[must_use]
    pub const fn new(observed_at: i64) -> Self {
        Self {
            bucket: None,
            resets_at: None,
            retry_after: None,
            source: Source::Reported,
            text: None,
            observed_at,
        }
    }
}

/// What one model of a session has used.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct ModelSpend {
    /// The model, as the harness names it.
    pub model: String,
    /// Input tokens not read from a cache.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from a cache.
    pub cache_read: u64,
    /// Input tokens written to a cache.
    pub cache_write: u64,
    /// Millionths of a US dollar, when the harness reported a cost; never `0` for unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
}

impl ModelSpend {
    /// Nothing spent yet on `model`.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cost_micro_usd: None,
        }
    }
}

/// What a session has spent: running totals.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct Spend {
    /// The session, by the id ACP reports.
    pub session_id: String,
    /// Input tokens not read from a cache.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from a cache.
    pub cache_read: u64,
    /// Input tokens written to a cache.
    pub cache_write: u64,
    /// The part of `output` spent reasoning, when the harness separates it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// Millionths of a US dollar, when the harness reported a cost; `None` when it did not or the
    /// figure is partial, never `0` for unknown. A conversion from another unit rounds to the
    /// nearest, halves up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
    /// The same by model, when the harness separates them.
    #[map(default)]
    #[serde(default)]
    pub by_model: Vec<ModelSpend>,
}

impl Spend {
    /// Nothing spent yet in `session_id`.
    #[must_use]
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reasoning: None,
            cost_micro_usd: None,
            by_model: Vec::new(),
        }
    }
}

/// What an account has used, as of the newest of its buckets.
///
/// `supported: false` is a real answer: this harness reports no usage. `supported` with no buckets
/// is a different one, nothing reported yet.
#[derive(ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct UsageReport {
    /// Whether the harness reports usage at all.
    pub supported: bool,
    /// Why not, when `supported` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_because: Option<String>,
    /// Which kinds of figure it can give at all, so a host knows an agent never reports figures.
    #[map(default)]
    #[serde(default)]
    pub sources: Vec<Source>,
    /// The host's id for the account the connection runs on, echoed and never interpreted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Grows by one with each change on a connection (and per provider instance for the kind's
    /// answer), so a reader that polls keeps the newest and an unchanged account costs nothing.
    pub revision: u64,
    /// The newest bucket's `observed_at`, in Unix seconds; `0` with no bucket.
    pub observed_at: i64,
    /// The allowances, each by its id.
    #[map(default)]
    #[serde(default)]
    pub buckets: Vec<Bucket>,
    /// The last refusal the report keeps, until its reset passes or a later report shows room.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<LimitHit>,
    /// What the session asked about has spent; absent without a session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend: Option<Spend>,
}

impl UsageReport {
    /// A supported harness's report with nothing reported yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            supported: true,
            unsupported_because: None,
            sources: Vec::new(),
            account: None,
            revision: 0,
            observed_at: 0,
            buckets: Vec::new(),
            limit: None,
            spend: None,
        }
    }

    /// The report of a harness that reports no usage, and why.
    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            supported: false,
            unsupported_because: Some(reason.into()),
            ..Self::new()
        }
    }

    /// How much of `bucket` is used, in thousandths and at most 1000: `used / limit`, else
    /// `(limit - remaining) / limit`, rounded down. `None` when the bucket gives neither pair or
    /// its limit is zero.
    #[must_use]
    pub fn fraction_permille(bucket: &Bucket) -> Option<u16> {
        let limit = bucket.limit.filter(|limit| *limit > 0)?;
        let used = bucket
            .used
            .or_else(|| bucket.remaining.map(|left| limit.saturating_sub(left)))?;
        let permille = (u128::from(used) * 1000 / u128::from(limit)).min(1000);
        u16::try_from(permille).ok()
    }
}

impl Default for UsageReport {
    fn default() -> Self {
        Self::new()
    }
}

/// What a harness can report, known before anything starts, in
/// [`HarnessInfo::usage`](crate::HarnessInfo::usage). Empty `sources` means none: the harness
/// answers [`USAGE_METHOD`] with `-32601`, advertises no capability, and its `usage` slot says
/// unsupported.
#[derive(
    ToValue, FromValue, Schema, Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq,
)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct UsageSupport {
    /// Which kinds of figure it can give.
    #[map(default)]
    #[serde(default)]
    pub sources: Vec<Source>,
    /// Whether it reports a session's [`Spend`].
    #[map(default)]
    #[serde(default)]
    pub spend: bool,
}

impl UsageSupport {
    /// Reports nothing.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            sources: Vec::new(),
            spend: false,
        }
    }

    /// Reports `sources`, and a session's spend when `spend`.
    #[must_use]
    pub const fn new(sources: Vec<Source>, spend: bool) -> Self {
        Self { sources, spend }
    }

    /// Whether it reports anything.
    #[must_use]
    pub fn reports(&self) -> bool {
        !self.sources.is_empty()
    }
}

/// What a host asks [`AcpHarness::usage`](crate::AcpHarness::usage): refuses a key it does not
/// declare.
#[derive(ToValue, FromValue, Schema, Debug, Clone, Default, PartialEq)]
#[map(crate = ::guatiao, deny_unknown_fields)]
#[non_exhaustive]
pub struct UsageQuery {
    /// Variables for what the harness runs to ask, over its configuration's. A provider shows none
    /// of them in an answer, a refusal, an error or a log.
    #[map(default)]
    pub environment: Secret<Environment>,
    /// The host's id for the account, echoed into the report. It never picks credentials: those
    /// stay in `environment`.
    pub account: Option<String>,
}

impl UsageQuery {
    /// A query adding nothing to the provider's configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The variables for what the harness runs.
    #[must_use]
    pub fn environment(mut self, environment: Environment) -> Self {
        self.environment = Secret::new(environment);
        self
    }

    /// The account the report is for.
    #[must_use]
    pub fn account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }
}
