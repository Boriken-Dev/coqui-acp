//! Account holds: placing a credential file, the events a look finds and who hears them, the commands a sign-in runs.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{Secret, Status};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, Credentials, FileInfo, LoginAnswer, LoginForm,
    LoginOutcome, LoginPrompter, OpenUrl, ProcessSpawner, SecretBytes, SpawnSpec, SpawnedProcess,
    SpawnerInfo,
};
use coqui_acp_harness::account::{
    Accounts, FileState, Files, Hold, Watched, jwt_expiry, rfc3339_ms,
};
use coqui_acp_harness::cli::{address_in, is_loopback};
use coqui_acp_harness::login::{reason, supervise};

/// What a read of a file answers, in order; the last answer repeats.
type Reads = VecDeque<Result<String, Status>>;

/// A spawner whose files are in memory and whose reads are scripted.
#[derive(Default)]
struct Scripted {
    files: Mutex<BTreeMap<String, Reads>>,
    written: Mutex<Vec<(String, String)>>,
    removed: Mutex<Vec<String>>,
    dropped: Arc<AtomicBool>,
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn refusal(status: Status, why: &str) -> ProviderError {
    ProviderError::new(status, why)
}

impl ProcessSpawner for Scripted {
    fn describe(&self) -> Result<SpawnerInfo, ProviderError> {
        let mut info = SpawnerInfo::new("test", "test", true);
        info.files = true;
        Ok(info)
    }

    fn spawn(&self, _spec: SpawnSpec) -> Result<Object<dyn SpawnedProcess>, ProviderError> {
        Err(refusal(Status::GUATIAO_ERR_UNSUPPORTED, "no programs"))
    }

    fn write_file(
        &self,
        path: &str,
        bytes: Secret<SecretBytes>,
        _private: bool,
    ) -> Result<(), ProviderError> {
        let text = bytes.expose().to_text();
        self.written
            .lock()
            .unwrap()
            .push((path.to_string(), text.clone()));
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), VecDeque::from([Ok(text)]));
        Ok(())
    }

    fn read_file(&self, path: &str, _max: u64) -> Result<Secret<SecretBytes>, ProviderError> {
        let mut files = self.files.lock().unwrap();
        let Some(reads) = files.get_mut(path) else {
            return Err(refusal(Status::GUATIAO_ERR_NOT_FOUND, "missing"));
        };
        let next = if reads.len() > 1 {
            reads.pop_front().unwrap()
        } else {
            reads.front().cloned().unwrap()
        };
        match next {
            Ok(text) => Ok(Secret::new(SecretBytes::new(text.into_bytes()))),
            Err(status) => Err(refusal(status, "refused")),
        }
    }

    fn file_info(&self, path: &str) -> Result<Option<FileInfo>, ProviderError> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .contains_key(path)
            .then(|| FileInfo::new(1, now_ms())))
    }

    fn remove_file(&self, path: &str) -> Result<(), ProviderError> {
        self.removed.lock().unwrap().push(path.to_string());
        self.files.lock().unwrap().remove(path);
        Ok(())
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

/// A spawner and the handles to look into it.
struct Fs {
    inner: Arc<Object<dyn ProcessSpawner>>,
    script: Arc<Scripted>,
}

impl Fs {
    fn new() -> Self {
        let script = Arc::new(Scripted::default());
        // The object owns a forwarding spawner so the test can still reach the script.
        let inner = Arc::new(Forward(Arc::clone(&script)).into_object());
        Self { inner, script }
    }

    fn files(&self) -> Files {
        Files::new(Arc::clone(&self.inner))
    }

    fn put(&self, path: &str, reads: &[&str]) {
        self.script.files.lock().unwrap().insert(
            path.to_string(),
            reads.iter().map(|r| Ok((*r).to_string())).collect(),
        );
    }

    fn put_refused(&self, path: &str, status: Status) {
        self.script
            .files
            .lock()
            .unwrap()
            .insert(path.to_string(), VecDeque::from([Err(status)]));
    }

    fn text(&self, path: &str) -> Option<String> {
        self.script
            .files
            .lock()
            .unwrap()
            .get(path)
            .and_then(|reads| reads.back().cloned())
            .and_then(Result::ok)
    }
}

struct Forward(Arc<Scripted>);

impl ProcessSpawner for Forward {
    fn describe(&self) -> Result<SpawnerInfo, ProviderError> {
        self.0.describe()
    }

    fn spawn(&self, spec: SpawnSpec) -> Result<Object<dyn SpawnedProcess>, ProviderError> {
        self.0.spawn(spec)
    }

    fn write_file(
        &self,
        path: &str,
        bytes: Secret<SecretBytes>,
        private: bool,
    ) -> Result<(), ProviderError> {
        self.0.write_file(path, bytes, private)
    }

    fn read_file(&self, path: &str, max: u64) -> Result<Secret<SecretBytes>, ProviderError> {
        self.0.read_file(path, max)
    }

    fn file_info(&self, path: &str) -> Result<Option<FileInfo>, ProviderError> {
        self.0.file_info(path)
    }

    fn remove_file(&self, path: &str) -> Result<(), ProviderError> {
        self.0.remove_file(path)
    }
}

/// What a keeper does when it hears a renewal, besides remembering it.
type Callback = Box<dyn Fn() + Send>;

#[derive(Clone, Default)]
struct Heard {
    renewed: Arc<Mutex<Vec<AccountRenewal>>>,
    lost: Arc<Mutex<Vec<AccountLoss>>>,
    on_renewed: Arc<Mutex<Option<Callback>>>,
}

impl AccountKeeper for Heard {
    fn renewed(&self, renewal: AccountRenewal) -> Result<(), ProviderError> {
        self.renewed.lock().unwrap().push(renewal);
        if let Some(act) = self.on_renewed.lock().unwrap().as_ref() {
            act();
        }
        Ok(())
    }

    fn lost(&self, loss: AccountLoss) -> Result<(), ProviderError> {
        self.lost.lock().unwrap().push(loss);
        Ok(())
    }
}

fn credentials(text: &str) -> Credentials {
    [("file", text)].into_iter().collect()
}

fn hold(accounts: &Arc<Accounts>, fs: &Fs, heard: &Heard, text: &str, revision: u64) -> Hold {
    accounts
        .hold(
            fs.files(),
            Some(heard.clone().into_object()),
            "acct",
            "/dir",
            revision,
            &credentials(text),
            vec![Watched::new("auth.json", "file")],
        )
        .expect("held")
}

#[test]
fn a_lost_login_is_reported_once_and_the_next_hold_writes_the_grant_back() {
    let accounts = Arc::new(Accounts::new());
    let fs = Fs::new();
    let heard = Heard::default();
    let first = hold(&accounts, &fs, &heard, "G1", 1);
    fs.put("auth.json", &[""]);
    first.look().unwrap();
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    first.finish();
    // The next hold finds the blank file: it is the grant's copy again, and the loss is not told a second time.
    let second = hold(&accounts, &fs, &heard, "G1", 1);
    assert_eq!(fs.text("auth.json").as_deref(), Some("G1"));
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    second.finish();
    assert!(heard.renewed.lock().unwrap().is_empty());
}

#[test]
fn a_file_the_writer_has_not_finished_is_neither_a_renewal_nor_a_loss() {
    let accounts = Arc::new(Accounts::new());
    let fs = Fs::new();
    let heard = Heard::default();
    let held = hold(&accounts, &fs, &heard, "G1", 1);
    // The first read sees half of the new login, the second all of it.
    fs.put("auth.json", &["{\"half", "{\"whole\":1}"]);
    held.look().unwrap();
    assert!(
        heard.renewed.lock().unwrap().is_empty(),
        "half a file is no renewal"
    );
    held.look().unwrap();
    let renewed = heard.renewed.lock().unwrap();
    assert_eq!(renewed.len(), 1);
    assert_eq!(
        renewed[0].credentials.expose().get("file"),
        Some("{\"whole\":1}")
    );
    drop(renewed);
    // A file just truncated, written again a moment later, is not lost.
    fs.put("auth.json", &["", "{\"whole\":2}"]);
    held.look().unwrap();
    assert!(heard.lost.lock().unwrap().is_empty());
}

#[test]
fn a_keeper_may_end_its_own_hold_from_the_call_that_tells_it() {
    let accounts = Arc::new(Accounts::new());
    let fs = Fs::new();
    let heard = Heard::default();
    let held = hold(&accounts, &fs, &heard, "G1", 1);
    let own = held.clone();
    *heard.on_renewed.lock().unwrap() = Some(Box::new(move || own.finish()));
    fs.put("auth.json", &["G2"]);
    let (done, finished) = std::sync::mpsc::channel();
    let looking = held.clone();
    std::thread::spawn(move || {
        let _ = looking.look();
        let _ = done.send(());
    });
    finished
        .recv_timeout(Duration::from_secs(10))
        .expect("the look came back: the keeper's call did not wait for itself");
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
}

#[test]
fn sequences_follow_the_order_the_files_changed_in_with_several_lookers() {
    let accounts = Arc::new(Accounts::new());
    let fs = Fs::new();
    let heard = Heard::default();
    let held = hold(&accounts, &fs, &heard, "v0", 1);
    let stop = Arc::new(AtomicBool::new(false));
    let lookers: Vec<_> = (0..3)
        .map(|_| {
            let held = held.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let _ = held.look();
                }
            })
        })
        .collect();
    for version in 1..=5 {
        fs.put("auth.json", &[&format!("v{version}")]);
        std::thread::sleep(Duration::from_millis(400));
    }
    stop.store(true, Ordering::SeqCst);
    for looker in lookers {
        looker.join().unwrap();
    }
    let mut seen: Vec<(u64, String)> = heard
        .renewed
        .lock()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.sequence,
                r.credentials.expose().get("file").unwrap().to_string(),
            )
        })
        .collect();
    seen.sort();
    let versions: Vec<&str> = seen.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(versions, ["v1", "v2", "v3", "v4", "v5"], "{seen:?}");
}

#[test]
fn a_login_for_another_account_is_refused_as_lost() {
    fn who(text: &str) -> Option<String> {
        text.strip_prefix("id:")
            .and_then(|rest| rest.split_whitespace().next())
            .map(str::to_string)
    }
    let accounts = Arc::new(Accounts::new());
    let fs = Fs::new();
    let heard = Heard::default();
    let held = accounts
        .hold(
            fs.files(),
            Some(heard.clone().into_object()),
            "acct",
            "/dir",
            1,
            &credentials("id:A tokens"),
            vec![Watched::new("auth.json", "file").identity(who)],
        )
        .unwrap();
    fs.put("auth.json", &["id:A renewed"]);
    held.look().unwrap();
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
    fs.put("auth.json", &["id:B someone else"]);
    held.look().unwrap();
    assert_eq!(
        heard.renewed.lock().unwrap().len(),
        1,
        "a foreign login is no renewal"
    );
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
}

#[test]
fn a_planted_file_is_replaced_by_the_grants_copy_rather_than_failing_the_hold() {
    let accounts = Arc::new(Accounts::new());
    let fs = Fs::new();
    let heard = Heard::default();
    // Longer than the spawner reads: it refuses with BAD_VALUE and the file is there.
    fs.put_refused("auth.json", Status::GUATIAO_ERR_BAD_VALUE);
    assert!(matches!(
        fs.files().state("auth.json").unwrap(),
        FileState::Planted
    ));
    let held = hold(&accounts, &fs, &heard, "G1", 1);
    assert!(
        fs.script
            .removed
            .lock()
            .unwrap()
            .contains(&"auth.json".to_string())
    );
    assert_eq!(fs.text("auth.json").as_deref(), Some("G1"));
    held.finish();
}

#[test]
fn a_changed_file_older_than_the_provider_is_not_news_and_a_newer_one_is() {
    let accounts = Arc::new(Accounts::new());
    let fs = Fs::new();
    let heard = Heard::default();
    // The scripted spawner stamps every file with the time now, which is after the provider began.
    fs.put("auth.json", &["{\"newer\":1}"]);
    let held = hold(&accounts, &fs, &heard, "G1", 1);
    assert_eq!(
        heard.renewed.lock().unwrap().len(),
        1,
        "a login changed since the provider began is news"
    );
    assert_eq!(fs.text("auth.json").as_deref(), Some("{\"newer\":1}"));
    held.finish();
}

#[test]
fn loopback_is_an_address_of_this_machine_by_what_it_parses_to() {
    for host in [
        "localhost",
        "LOCALHOST.",
        "127.0.0.1",
        "127.8.9.1",
        "::1",
        "0.0.0.0",
        "app.localhost",
    ] {
        assert!(is_loopback(host), "{host}");
    }
    for host in [
        "localhost.example.com",
        "127.0.0.1.example.com",
        "10.0.0.1",
        "example.com",
    ] {
        assert!(!is_loopback(host), "{host}");
    }
}

#[test]
fn a_jwt_says_when_it_ends_and_a_time_is_written_as_rfc_3339() {
    // {"alg":"none"}.{"exp":4102444800}.
    let token = "eyJhbGciOiJub25lIn0.eyJleHAiOjQxMDI0NDQ4MDB9.";
    assert_eq!(jwt_expiry(token).as_deref(), Some("2100-01-01T00:00:00Z"));
    assert_eq!(jwt_expiry("not-a-jwt"), None);
    assert_eq!(rfc3339_ms(0), "1970-01-01T00:00:00Z");
}

#[test]
fn only_a_page_the_person_can_open_is_an_address_to_open() {
    let local = "Starting local login server on http://localhost:1455.\nIf your browser did not open, visit:\nhttps://auth.openai.com/oauth/authorize?x=1\n\n";
    assert_eq!(
        address_in(local, &[]).as_deref(),
        Some("https://auth.openai.com/oauth/authorize?x=1")
    );
    for loopback in [
        "http://127.0.0.2:80/a ",
        "http://[::1]:9/a ",
        "http://localhost./a ",
        "http://0.0.0.0/a ",
        "http://a@localhost/a ",
    ] {
        assert_eq!(address_in(loopback, &[]), None, "{loopback}");
    }
    // A hyperlink to this machine is no more a page than its text is.
    assert_eq!(
        address_in(
            "\u{1b}]8;;http://127.0.0.1:1455/cb\u{1b}\\go\u{1b}]8;;\u{1b}\\\n",
            &[]
        ),
        None
    );
    // A command's output cannot send the person to a host the kind does not use.
    let evil = "visit https://evil.example/login now\n";
    assert_eq!(address_in(evil, &["claude.com"]), None);
    assert_eq!(
        address_in(evil, &[]).as_deref(),
        Some("https://evil.example/login")
    );
    assert_eq!(
        address_in(
            "visit https://platform.claude.com/oauth/authorize?a=1 now",
            &["claude.com"]
        )
        .as_deref(),
        Some("https://platform.claude.com/oauth/authorize?a=1")
    );
    assert_eq!(
        address_in("visit https://notclaude.com/x now", &["claude.com"]),
        None
    );
}

#[test]
fn a_reason_echoes_nothing_the_person_typed_whole_or_in_part_and_no_address() {
    let key = "sk-live-abcdef0123456789";
    assert_eq!(
        reason("Error: bad key sk-live-abcdef0123456789", &[key]),
        "it was not accepted"
    );
    assert_eq!(reason("Error: bad key sk-l", &[key]), "it was not accepted");
    assert_eq!(
        reason("Error: bad key ...6789", &[key]),
        "it was not accepted"
    );
    assert_eq!(
        reason("Error: see example.com/help/keys", &[key]),
        "Error: see"
    );
    assert_eq!(
        reason("Error: see https://x.example/help", &[key]),
        "Error: see"
    );
    assert_eq!(
        reason("Error: invalid agent identity JWT format", &[key]),
        "Error: invalid agent identity JWT format"
    );
    // Another value typed beside it (a code, an address) is held back the same way.
    assert_eq!(
        reason("Error: code 4f9a-77c1 refused", &[key, "4f9a-77c1"]),
        "it was not accepted"
    );
}

struct Cancelling;

impl LoginPrompter for Cancelling {
    fn say(&self, _text: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    fn open_url(&self, _open: OpenUrl) -> Result<(), ProviderError> {
        Ok(())
    }

    fn ask(&self, _form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        Ok(LoginAnswer::default())
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(true)
    }
}

#[test]
fn a_cancelled_sign_in_has_let_go_of_the_spawner_when_it_answers() {
    let script = Arc::new(Scripted::default());
    let dropped = Arc::clone(&script.dropped);
    let spawner = Arc::new(Forward(script).into_object());
    let answer = supervise(
        spawner,
        Cancelling.into_object(),
        60_000,
        Box::new(|_spawner, _prompter, _job| {
            // The work takes a while to notice and then lets go.
            std::thread::sleep(Duration::from_millis(400));
            Err(ProviderError::new(Status::GUATIAO_ERR_GONE, "ended"))
        }),
    );
    let error: ProviderError = answer.map(|_: LoginOutcome| ()).expect_err("cancelled");
    assert_eq!(error.status, Status::GUATIAO_ERR_GONE);
    assert!(error.message().contains("cancelled"));
    assert!(
        dropped.load(Ordering::SeqCst),
        "the spawner was still held when the answer came"
    );
}
