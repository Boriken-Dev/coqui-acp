# `coqui-acp-pool` — API header

Header-file-style reference for the `coqui-acp-pool` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. The design (what a host supplies, the order of the checks, how a session moves) is in `docs/pools.md` of <https://github.com/Boriken-Dev/coqui-acp>.

Cargo feature `std` (default): `SharedMemory` and `SharedCache`, behind a mutex. Without it the crate is `no_std` plus `alloc`. MSRV Rust 1.89. Dependencies: `coqui-acp-api` (usage and capability types, the limit-phrase reader), `coqui-acp-history` (the exported history the cleaner reads, `LossReport`, `TurnRecord`) and `serde_json`. No `unsafe` (`#![forbid(unsafe_code)]`). Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

What it is not: no I/O, no file, no secret, no clock (every `now` is Unix seconds, passed in), no Canoa or AI6 type, no list of harnesses, accounts, standards or capabilities. A host builds `Pool<T>` values from its own configuration and implements the seams below. Every bound is a `Settings` value with a default and no floor.

## Pool

```rust
pub struct Pool<T> { pub name: String, pub strategy: Strategy, pub settings: Settings, pub members: Vec<Member<T>> }   // non_exhaustive
Pool::new(name) -> Pool<T>;  .strategy(Strategy) .settings(Settings) .member(Member<T>);  .position(&str) -> Option<usize>;  .get(&str) -> Option<&Member<T>>
pub struct Member<T> { pub id: String, pub payload: T, pub group: String, pub priority: i32, pub max_concurrent: Option<u32>,
                       pub stop_permille: Option<u16>, pub labels: Vec<String> }   // non_exhaustive
Member::new(id, payload);  .group(..) .priority(i32) .max_concurrent(u32) .stop_permille(u16) .label(..)
pub enum Strategy { Priority /*default*/, Headroom, RoundRobin }   // name() / Display / FromStr: "priority", "headroom", "round_robin"
pub struct Settings { pub unknown_permille: u16 /*500*/, pub stop_permille: u16 /*1000*/, pub max_concurrent: Option<u32> /*None*/,
                      pub max_name_chars: usize /*64*/, pub max_members: usize /*256*/ }   // builders of the same names; non_defaults() -> Vec<NonDefault>
```

`T` is the host's payload (an agent entry with its account, model, effort and profile as names; an AI6 route); the library never looks inside. `group` is opaque: a session that moves prefers members of its current member's group (Canoa: the harness). A member's own `max_concurrent` and `stop_permille` win over the settings'. Member ids are unique in a pool.

## Deciding

```rust
pub fn decide<T>(pool: &Pool<T>, memory: &mut Memory, standings: &[Standing], ask: &Ask<'_>) -> Result<Chosen, NoneLeft>;
pub struct Ask<'a> { pub current: Option<&'a str>, pub model: Option<&'a str>, pub idle: Option<Duration>, pub lifetime: CacheLifetime,
                     pub now: i64, pub stick: bool /*true*/, pub session: Option<&'a str> }   // Ask::new(now); .current .model .idle .lifetime .stick .session
pub struct Standing { pub unavailable: Option<String>, pub usage: Usage, pub limited_until: Option<i64>, pub holds: Vec<Hold>,
                      pub active: u32, pub last_used: Option<i64>, pub carries: bool }   // Standing::new() (default); .unavailable .usage .limited_until .hold .active .last_used .carries
pub enum Usage { Known(UsageReport), Unknown /*default*/, Pending }
pub struct Hold { pub reason: String, pub until: Option<i64> }   // Hold::new(reason).until(t)
pub struct Chosen { pub member: String, pub index: usize, pub moved: bool, pub why: Why, pub left: Option<String>, pub usage_known: bool,
                    pub usage_pending: bool, pub same_group: bool, pub skipped: Vec<Skipped> }
pub enum Why { Start, Kept, InTurn, Exhausted }   // name(): "start", "kept", "in_turn", "exhausted"
pub struct NoneLeft { pub pool: String, pub model: Option<String>, pub reasons: Vec<Skipped>, pub soonest: Option<i64>, pub now: i64 }   // Error + Display; message(noun)
pub struct Skipped { pub member: String, pub reason: Reason }
pub enum Reason { Unavailable(String), AtCapacity { active, max }, Refused { until }, UsedUp { bucket, level: Option<(u16, u16)>, until }, Held { reason, until } }   // text(), until(), is_allowance()
```

`standings` has one entry per member in the pool's order; a missing one counts as a member with nothing against it. `Standing::active` is the runs on the member **not counting the asking session's own** (so a session alone on a member with a cap of 1 is not held out of it).

Checks, in order, the first a member fails is its `Reason`: unavailable (the host's: not installed, not signed in, held by another connection, model not listed, capability missing, not offered); at its cap (`active >= max_concurrent`); a turn a limit refused that has not lifted (`limited_until`; `i64::MAX` when no time was said); a bucket that covers the model, applies (`Status::Disabled` does not) and whose window has not ended, with status `Exhausted` or used at or past the member's `stop_permille`; a `Hold` not yet over. A bucket that names models and an `Ask` with no model do not meet. Use is a bucket's `used/limit` or `(limit - remaining)/limit` in permille (`UsageReport::fraction_permille`), the most used covering bucket counting. **Unknown usage** (no report, `Pending`, or no bucket covering the model) leaves a member eligible with `usage_known = false`; `usage_pending` says a report was asked for and has not come; the library never waits.

Order among members that can serve: `Priority`: highest `priority`, ties in the listed order. `Headroom`: least used (unknown counts as `Settings::unknown_permille`), then fewest `active`, then least recently used (the later of `Standing::last_used` and what `Memory` recorded; never used first), then priority, then the listed order. `RoundRobin`: the member after the last one the pool gave a turn (`Memory`), else after the current one. With a current member, its group's members come before any other group's, each part in the strategy's order. A session whose member can serve and that `stick`s stays (`Why::Kept`), except under `RoundRobin`, where it stays only while its prompt cache is warm (`idle < lifetime_span(lifetime)`, `Off` never warm, no `idle` cold). Otherwise the strategy's first is `Start` (no current), `Exhausted` (the current cannot serve; `left` says what held it back, unless it was unavailable) or `InTurn`. `NoneLeft::message("account")` is the refusal: each member's reason, and the soonest refill (`after`, `utc`) or "nothing says when an allowance refills" when an allowance holds one back.

```rust
pub struct Memory;   // Default, Clone; last(pool), last_used(pool, member), took(pool, member, at), touch(..), forget(pool), export() -> Vec<Remembered>, restore(iter)
pub trait Keeper { fn keep(&self, with: &mut dyn FnMut(&mut Memory)); }   // implemented for RefCell<Memory> and (std) SharedMemory
pub fn lifetime_span(CacheLifetime) -> Duration;  pub fn after(seconds: i64) -> String;  pub fn utc(seconds: i64) -> String;   // "in 2 h 10 min", "2027-01-15 08:00 UTC"
pub fn fits<N: AsRef<str>, C: AsRef<str>>(needs: &[N], capabilities: &[C]) -> Fit;   // Yes | No(missing) | Unreported (a model reporting none counts as able; Fit::able())
pub struct Cache<T>;   // new(life_secs), .max_entries(n) (1024), get(key, now) -> Option<&T>, put(key, value, now), forget() -> usize; SharedCache<T: Clone> (std) owns the lock
```

## The switchboard and the seams

```rust
Switchboard::new(&pool, &dyn Keeper, &dyn Availability<T>, &dyn Starter<T>);  .usage(&dyn UsageSource<T>) .occupancy(&dyn Occupancy<T>) .holds(&dyn Holds<T>) .ledger(&dyn Ledger)
fn assign(&self, ask: &Ask<'_>, record: TurnRecord) -> Result<Assigned, Unassigned>;   // decide, Starter::start; a start that fails leaves the member out and decides again, once per member
fn resolve(&self, ask: &Ask<'_>) -> Result<Chosen, NoneLeft>;                           // what it would pick, nothing started, the cursor not moved
fn choose_next(&self, ended: &Ended<'_>) -> Result<Option<Chosen>, NoneLeft>;           // where at_turn would move the session (None: it stays); no carry, no memory
fn at_turn(&self, ended: &Ended<'_>) -> AtTurn;                                         // Stayed | Moved(Box<Moved>) | NoneLeft(NoneLeft) | Unknown(member id)
pub enum HowEnded { Limit { until: Option<i64> }, Finished, Other }
pub struct Ended<'a> { session, current, model, now, how, resume_text, record }          // Ended::new(session, current, how, now); .model .resume_text .record
pub struct Moved { pub from: String, pub chosen: Chosen, pub replayed: bool, pub loss: LossReport, pub resume: Resume, pub resume_error: Option<String>, pub limit_text_left: bool }
```

`at_turn`: `Other` moves nothing. The session's member is held out for what ran out (`Limit`: until the stated time, else for good; the `Finished` case relies on the standing), `stick` and a warm cache are assumed, and members whose `Standing::carries` is false (a harness that can neither export nor import) are left out for a running session. A session moves only when its member cannot serve, never because an earlier member refilled, and never mid-turn (it is called when a turn ends). On a move: `Starter::carry`; `Refused` leaves that member out for this session and decides again; then `resume` (`Resume::Nothing` after a `Finished` turn). `Moved::chosen.same_group` false is a move to another group (harness): a host that asks the person first uses `choose_next` and then `at_turn`.

```rust
pub trait UsageSource<T> { fn report(&self, member: &Member<T>) -> Usage; }                       // never awaited; Pending is a report still to come
pub trait Occupancy<T>   { fn active(&self, member, excluding: Option<&str>) -> u32; fn last_used(&self, member) -> Option<i64>; }
pub trait Holds<T>       { fn holds(&self, member, now: i64) -> Vec<Hold>; }                          // the host decides the scope (account, profile, operation, glob)
pub trait Availability<T>{ fn availability(&self, member, ask: &Ask<'_>) -> Basics; }               // Basics { unavailable, limited_until, carries }; Basics::new().unavailable(..).limited_until(..).cannot_carry()
pub trait Starter<T> {
    fn start(&self, member: &Member<T>, ask: &Ask<'_>) -> Result<Started, String>;                  // Started::new(session)
    fn carry(&self, session: &str, from: &Member<T>, to: &Member<T>) -> Carry;                      // export, clean, import: the host's
    fn resume(&self, session: &str, to: &Member<T>, resume: &Resume) -> Result<(), String>;
}
pub enum Carry { Whole(Carried), Replayed(Carried), Refused(String) }   // Carried::new(loss, had_work).prompt(blocks).limit_text_left()
pub trait Ledger { fn record(&self, record: &TurnRecord); }
```

`Availability` is a sixth seam beyond the five of the design: what the host knows that is neither usage, runs nor a hold. The switchboard does not ask `UsageSource`, `Holds` or `Occupancy` about an unavailable member. A `Ledger` receives a `TurnRecord` on every successful `assign` and every move; the switchboard fills `pool`, `member`, `why`, `usage_known`, `session`, `at`, `moved_from` and `outcome.moved_on`/`limit`, and `harness` (the member's group) and `model` when the host left them out; the host fills the rest (`agent`, `account`, `effort`, `role`, `profile`, `tokens`, `cost`, `duration_ms`, `tags`).

## The cleaner and the continuation

```rust
pub fn clean(document: &Document, cleaning: &Cleaning<'_>) -> Cleaned;
pub struct Cleaning<'a> { pub phrases: &'a [&'a str], pub notice_key: Option<&'a str>, pub ended_by_limit: bool /*true*/, pub max_window: usize /*64*/ }   // Cleaning::new(phrases); .notice_key(..) .ended_by_limit(bool) .max_window(n)
pub struct Cleaned { pub document: Document, pub removed: Removed, pub had_work: bool, pub prompt: Option<Vec<Value>>, pub limit_text_left: bool }   // .resume(role_text, ended_by_limit)
pub struct Removed { pub notices: usize, pub end: bool, pub limit_text: usize, pub open_calls: usize }
pub enum Resume { Nothing, Continue(String), Repeat(Vec<Value>) }
pub fn resume(role_text: Option<&str>, (had_work, prompt): (bool, Option<&[Value]>), ended_by_limit: bool) -> Resume;
pub const DEFAULT_CONTINUATION: &str;   // "The previous agent's usage ran out; continue this session where it stopped."
```

`clean` works on the exported history and only on its last turn (from the last `session/prompt`); earlier turns are not touched. `phrases` are the host's (`coqui_acp_api::DEFAULT_LIMIT_PHRASES`, or the person's), matched without regard to ASCII case by `infer_limit`. With `ended_by_limit` it drops: the closing `error` line or `refusal` stop; the agent's message and thought chunks after the last tool call, plan or permission line, from the first run of up to `max_window` chunks that together name a limit (agents stream, so a phrase may span chunks) to the end ("please wait" included); tool calls with no `completed`/`failed` update, with the permission requests and answers about them. Always, whole history: every line whose `_meta` (the line's, or its update's) holds `notice_key`. Text before the last tool call is never judged, so an agent discussing "quota" in its work is safe. The turn is left open when its end went (an import closes it as cancelled). `limit_text_left`: a limit ended the turn and nothing recognisable was removed; tell the person "limit text not removed". `had_work`: after cleaning the turn holds an agent message, thought, plan or tool call. `resume`: `Nothing` when the turn ended well; the person's own `prompt` again (`Repeat`) when no work was done and the prompt is known; else `Continue` with the role's text (blank counts as unset) or the default line. Nothing else (no summary, account, reset time, instruction) goes into the turn.

## Account pools (`accounts`) and checks

```rust
pub trait Provides { fn accounts(&self) -> Vec<String>; fn kind_of(&self, account: &str) -> Option<(String, Vec<String>)>; fn logins(&self) -> Vec<String>; }
pub const AUTOMATIC: &str /* "any-" */;  fn automatic_standard(name) -> Option<&str>;  fn automatic_name(standard) -> String;  const fn worth_listing(automatic, accounts: usize) -> bool;
fn misfit(&dyn Provides, standard, member) -> Result<(), String>;  fn check_one(&dyn Provides, &Declared) -> Result<(), String>;
fn listing(&[Declared], &dyn Provides) -> Vec<AccountPool>;  fn listed(..) -> Vec<AccountPool>;   // `any-<standard>` for each standard some account (or the login behind `local`) provides; `listed` drops those with fewer than two
pub struct Declared { name, standard, accounts }; pub struct AccountPool { name, standard, automatic, accounts };
pub struct EntryPool { name, standard, automatic, listed, members: Vec<EntryMember> }  // .usable(any) .open_to(any);  EntryMember { name, listed }
pub fn check_pool<S: AsRef<str>>(name, standard: Option<&str>, members: &[S], known: &dyn Known, &Settings) -> Result<(), PoolIssue>;
pub trait Known { fn taken(&self, name) -> bool; fn member(&self, name) -> bool; }
pub enum PoolIssue { InvalidLabel { max }, NameTaken(String), AutomaticPool(String), InvalidStandard, UnknownMember(String), DuplicateMember(String), TooManyMembers { max } }   // Display, Error
```

`Provides::kind_of` answers configured accounts; the automatic `local` is answered by the library from `logins()` (kind `harness-login`). `check_pool` order: name shape, another thing's name (`Known::taken`), a host pool's name and the standard (only with a `standard`; a role has none), member count, then each member's existence and repetition.

## The turn record (re-exported from `coqui-acp-history`)

`TurnRecord`, `Tokens`, `Cost`, `TurnOutcome`, `TagError`: serde types; unknown fields are `None`, never zero. Tags are `namespace:name` (`acp:` for `_meta` the agent returned, `canoa:`, `ai6:`, any host's own): `tag(key, value)`, `tags_of(namespace)`, `tag_acp_meta(&meta)`.
