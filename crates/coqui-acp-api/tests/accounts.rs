//! The Rust echo's accounts, called as the kind with a spawner that serves a real directory: who
//! hears a rotation, what the files say when nobody has looked yet, and a look that traffic cannot
//! starve.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Secret, Status, Value};
use coqui_acp_api::{
    AccountGrant, AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, ChannelSink, Credentials,
    FileInfo, ModelsRequest, PrivateDir, ProcessSpawner, RenewRequest, SecretBytes, SpawnSpec,
    SpawnedProcess, SpawnerInfo, StartRequest,
};

fn library() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    let profile = tests
        .parent()
        .and_then(Path::parent)
        .expect("a test binary is in target/<profile>/deps");
    profile.join("examples").join(format!(
        "{}echo_harness{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

fn echo(registry: &Registry, settings: Map) -> Arc<dyn AcpHarness> {
    let offer = registry
        .offer::<dyn AcpHarness>("echo_harness")
        .expect("the library offers it")
        .expect("its table is this crate's kind");
    Arc::new(
        offer
            .instantiate(&Value::from(settings))
            .expect("the settings fit the schema"),
    )
}

fn registry() -> Registry {
    let mut registry = Registry::new("coqui-acp-api-accounts-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once `cargo build --examples` built it");
    registry
}

/// A spawner serving one directory, as a host's does.
struct Files(PrivateDir);

impl ProcessSpawner for Files {
    fn describe(&self) -> Result<SpawnerInfo, ProviderError> {
        let mut info = SpawnerInfo::new(std::env::consts::OS, std::env::consts::ARCH, true);
        info.files = true;
        Ok(info)
    }

    fn spawn(&self, _spec: SpawnSpec) -> Result<Object<dyn SpawnedProcess>, ProviderError> {
        Err(ProviderError::new(
            Status::GUATIAO_ERR_UNSUPPORTED,
            "no programs here",
        ))
    }

    fn write_file(
        &self,
        path: &str,
        bytes: Secret<SecretBytes>,
        private: bool,
    ) -> Result<(), ProviderError> {
        self.0.write(path, bytes.expose().as_slice(), private)
    }

    fn read_file(&self, path: &str, max_bytes: u64) -> Result<Secret<SecretBytes>, ProviderError> {
        self.0
            .read(path, max_bytes)
            .map(|bytes| Secret::new(SecretBytes::new(bytes)))
    }

    fn file_info(&self, path: &str) -> Result<Option<FileInfo>, ProviderError> {
        self.0.info(path)
    }

    fn remove_file(&self, path: &str) -> Result<(), ProviderError> {
        self.0.remove(path)
    }
}

fn spawner(dir: &Path) -> Object<dyn ProcessSpawner> {
    Files(PrivateDir::new(dir)).into_object()
}

/// What a keeper was told.
#[derive(Default, Clone)]
struct Told(Arc<Mutex<(Vec<AccountRenewal>, Vec<AccountLoss>)>>);

impl Told {
    fn keeper(&self) -> Object<dyn AccountKeeper> {
        Records(self.clone()).into_object()
    }

    fn renewed(&self) -> Vec<AccountRenewal> {
        self.0.lock().expect("not poisoned").0.clone()
    }

    fn lost(&self) -> Vec<AccountLoss> {
        self.0.lock().expect("not poisoned").1.clone()
    }
}

struct Records(Told);

impl AccountKeeper for Records {
    fn renewed(&self, renewal: AccountRenewal) -> Result<(), ProviderError> {
        (self.0).0.lock().expect("not poisoned").0.push(renewal);
        Ok(())
    }

    fn lost(&self, loss: AccountLoss) -> Result<(), ProviderError> {
        (self.0).0.lock().expect("not poisoned").1.push(loss);
        Ok(())
    }
}

/// A sink that takes everything.
struct Takes;

impl ChannelSink for Takes {
    fn receive(&mut self, _bytes: &[u8]) -> Result<(), ProviderError> {
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
}

fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acp-accounts-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("a directory");
    dir
}

fn grant(dir: &Path, text: &str) -> AccountGrant {
    AccountGrant::new("account-1", "echo-login")
        .credentials(Credentials::from_iter([("auth_file", text)]))
        .state_dir(dir.to_string_lossy())
        .revision(3)
}

fn models(harness: &dyn AcpHarness, dir: &Path, grant: &AccountGrant, told: &Told) {
    harness
        .models(
            ModelsRequest::new().account(grant.clone()),
            spawner(dir),
            told.keeper(),
        )
        .expect("answered");
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "{what} did not happen");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A file there before this instance began is older than the grant and is written over; one
/// changed since is the harness's news, is kept, and is told.
#[test]
fn a_file_found_with_no_record_is_the_grants_or_news() {
    let registry = registry();

    let old = dir();
    std::fs::write(old.join("auth.json"), "stale").expect("written");
    std::thread::sleep(Duration::from_millis(250));
    let harness = echo(&registry, Map::new());
    let told = Told::default();
    models(&*harness, &old, &grant(&old, "from-the-host"), &told);
    assert_eq!(
        std::fs::read_to_string(old.join("auth.json")).expect("read"),
        "from-the-host"
    );
    assert!(told.renewed().is_empty() && told.lost().is_empty());

    let fresh = dir();
    let harness = echo(&registry, Map::new());
    std::fs::write(fresh.join("auth.json"), "rotated-by-the-harness").expect("written");
    let told = Told::default();
    let account = grant(&fresh, "from-the-host");
    models(&*harness, &fresh, &account, &told);
    assert_eq!(
        std::fs::read_to_string(fresh.join("auth.json")).expect("read"),
        "rotated-by-the-harness",
        "a newer file was written over"
    );
    let renewed = told.renewed();
    assert_eq!(renewed.len(), 1, "{renewed:?}");
    assert_eq!(renewed[0].from_revision, 3);
    assert_eq!(
        renewed[0].credentials.expose().get("auth_file"),
        Some("rotated-by-the-harness")
    );

    // The record is made, so the next rotation is seen too, and counted after the first.
    std::fs::write(fresh.join("auth.json"), "rotated-again").expect("written");
    models(&*harness, &fresh, &account, &told);
    let renewed = told.renewed();
    assert_eq!(renewed.len(), 2, "{renewed:?}");
    assert!(renewed[1].sequence > renewed[0].sequence);
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(&fresh);
}

/// A rotation reaches every keeper live for the account, with one sequence, whichever noticed it.
#[test]
fn every_live_keeper_hears_a_rotation() {
    let registry = registry();
    let harness = echo(&registry, Map::new());
    let dir = dir();
    let account = grant(&dir, "from-the-host");
    let connection = Told::default();
    let channel = harness
        .start(
            StartRequest::builder().account(account.clone()).build(),
            spawner(&dir),
            Takes.into_object(),
            connection.keeper(),
        )
        .expect("started");
    std::fs::write(dir.join("auth.json"), "rotated").expect("written");
    let call = Told::default();
    models(&*harness, &dir, &account, &call);
    assert_eq!(call.renewed().len(), 1, "the call that noticed it heard it");
    until("the connection's keeper to hear it", || {
        !connection.renewed().is_empty()
    });
    let (one, two) = (&connection.renewed()[0], &call.renewed()[0]);
    assert_eq!(one.sequence, two.sequence);
    assert_eq!(connection.renewed().len(), 1);
    channel.close(0, "");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Steady traffic does not keep a connection from looking at the account's file.
#[test]
fn traffic_does_not_starve_the_look() {
    let registry = registry();
    let mut settings = Map::new();
    settings.set("renew_poll_ms", 100);
    let harness = echo(&registry, settings);
    let dir = dir();
    let account = grant(&dir, "from-the-host");
    let connection = Told::default();
    let channel = harness
        .start(
            StartRequest::builder().account(account).build(),
            spawner(&dir),
            Takes.into_object(),
            connection.keeper(),
        )
        .expect("started");
    std::fs::write(dir.join("auth.json"), "rotated").expect("written");
    let deadline = Instant::now() + Duration::from_secs(20);
    while connection.renewed().is_empty() {
        assert!(
            Instant::now() < deadline,
            "20 s of traffic and the file was not looked at"
        );
        channel
            .send(br#"{"jsonrpc":"2.0","method":"noop"}"#)
            .expect("taken");
        std::thread::sleep(Duration::from_millis(10));
    }
    channel.close(0, "");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A blank or missing login file is lost, and `renew` of it is GONE: nothing is made of it.
#[test]
fn renew_of_a_lost_login_is_gone() {
    let registry = registry();
    let harness = echo(&registry, Map::new());
    let dir = dir();
    let account = grant(&dir, "from-the-host");
    let told = Told::default();
    models(&*harness, &dir, &account, &told);
    let renew =
        |account: &AccountGrant| harness.renew(RenewRequest::new(account.clone()), spawner(&dir));
    let renewed = renew(&account).expect("a login on disk renews");
    assert!(
        renewed
            .credentials
            .expect("new credentials")
            .expose()
            .get("auth_file")
            .is_some_and(|text| text.ends_with("#renewed"))
    );
    std::fs::write(dir.join("auth.json"), "  \n").expect("blanked");
    assert_eq!(
        renew(&account).expect_err("blank").status,
        Status::GUATIAO_ERR_GONE
    );
    std::fs::remove_file(dir.join("auth.json")).expect("removed");
    assert_eq!(
        renew(&account).expect_err("missing").status,
        Status::GUATIAO_ERR_GONE
    );
    assert!(
        !dir.join("auth.json").exists(),
        "renew wrote a login file back"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
