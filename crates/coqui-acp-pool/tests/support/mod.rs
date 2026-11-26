//! Helpers the test binaries share: stand-in pools, standings and usage.

#![allow(dead_code, reason = "each test binary uses some of these")]

use coqui_acp_api::{Bucket, Coverage, Refill, Status, UsageReport};
use coqui_acp_pool::{
    Ask, Chosen, Member, Memory, NoneLeft, Pool, Standing, Strategy, Usage, decide,
};

pub const NOW: i64 = 1_800_000_000;

/// A report with one five-hour bucket.
pub fn report(
    status: Status,
    models: Coverage,
    resets_at: Option<i64>,
    used: Option<(u64, u64)>,
) -> UsageReport {
    let mut bucket = Bucket::new("five_hour");
    bucket.status = status;
    bucket.models = models;
    bucket.refill = Refill::FixedWindow {
        length: None,
        resets_at,
    };
    if let Some((used, limit)) = used {
        bucket.used = Some(used);
        bucket.limit = Some(limit);
    }
    let mut report = UsageReport::new();
    report.supported = true;
    report.buckets = vec![bucket];
    report
}

/// A standing whose provider reports one bucket.
pub fn reporting(status: Status, models: Coverage, resets_at: Option<i64>) -> Standing {
    Standing::new().usage(Usage::Known(report(status, models, resets_at, None)))
}

/// A standing that is used up until `resets_at`.
pub fn exhausted(resets_at: i64) -> Standing {
    reporting(Status::Exhausted, Coverage::Unknown, Some(resets_at))
}

/// A standing that has used `permille` thousandths of a window that ends well after `NOW`.
pub fn used(permille: u64) -> Standing {
    Standing::new().usage(Usage::Known(report(
        Status::Ok,
        Coverage::All,
        Some(NOW + 3600),
        Some((permille, 1000)),
    )))
}

pub fn pool_of(name: &str, strategy: Strategy, ids: &[&str]) -> Pool<()> {
    let mut pool = Pool::new(name).strategy(strategy);
    for id in ids {
        pool = pool.member(Member::new(*id, ()));
    }
    pool
}

pub fn take(pool: &Pool<()>, memory: &mut Memory, standings: &[Standing], ask: &Ask<'_>) -> Chosen {
    decide(pool, memory, standings, ask).expect("a member takes it")
}

pub fn refused(pool: &Pool<()>, standings: &[Standing], ask: &Ask<'_>) -> NoneLeft {
    decide(pool, &mut Memory::new(), standings, ask).expect_err("none can take it")
}
