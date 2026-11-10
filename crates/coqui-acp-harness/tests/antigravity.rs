//! Antigravity's quota read from stand-in servers on loopback: plain HTTP with chunked framing, TLS
//! with a certificate made here, and the answers and failures a hostile or odd server gives.
//!
//! The reader finds a port by the owner of its socket, and the stand-ins are this process's own, so
//! the tests run one at a time and each ends its servers.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use coqui_acp_api::usage::{Bucket, Coverage, Refill, Source, Status, Unit};
use coqui_acp_harness::antigravity::{
    AcpModel, AgyQuota, ReadError, ReadLimits, loopback_listeners,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const FIXTURE: &str = include_str!("fixtures/agy-1.5.3/getuserstatus.json");
const FIVE_HOURS: i64 = 18_000;
const SEVEN_DAYS: i64 = 604_800;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn me() -> Vec<u32> {
    vec![std::process::id()]
}

fn unix_now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .unwrap()
        .format(&Rfc3339)
        .unwrap()
}

/// The recorded answer with each model's reset a window from now: seven days for the Gemini 4
/// Argon models, five hours for the others. `edit` changes the model list.
fn answer(edit: impl FnOnce(&mut Vec<Value>)) -> Vec<u8> {
    let mut status: Value = serde_json::from_str(FIXTURE).unwrap();
    let now = unix_now();
    let models = status["userStatus"]["cascadeModelConfigData"]["clientModelConfigs"]
        .as_array_mut()
        .unwrap();
    for model in models.iter_mut() {
        let window = if model["label"]
            .as_str()
            .unwrap()
            .starts_with("Gemini 4 Argon")
        {
            SEVEN_DAYS
        } else {
            FIVE_HOURS
        };
        model["quotaInfo"]["resetTime"] = json!(rfc3339(now + window));
    }
    edit(models);
    serde_json::to_vec(&status).unwrap()
}

/// The ACP agent's models for the recorded answer.
fn acp_models() -> Vec<AcpModel> {
    let status: Value = serde_json::from_str(FIXTURE).unwrap();
    status["userStatus"]["cascadeModelConfigData"]["clientModelConfigs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| {
            AcpModel::new(
                model["modelId"].as_str().unwrap(),
                model["label"].as_str().unwrap(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Stand-in servers.

struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accepted: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Listens on loopback and gives each connection to `handle` on a thread of its own.
    fn start(handle: impl Fn(TcpStream, &AtomicBool) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let accepted = Arc::new(AtomicUsize::new(0));
        let handle = Arc::new(handle);
        let (stopping, counting) = (Arc::clone(&stop), Arc::clone(&accepted));
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                counting.fetch_add(1, Ordering::SeqCst);
                let (handle, stopping) = (Arc::clone(&handle), Arc::clone(&stopping));
                thread::spawn(move || handle(stream, &stopping));
            }
        });
        Self {
            address,
            stop,
            accepted,
            thread: Some(thread),
        }
    }

    /// Answers every plain HTTP request with `status` and `body`.
    fn plain(status: &'static str, body: Vec<u8>, chunked: bool) -> Self {
        Self::start(move |mut stream, _| {
            if is_tls(&stream) {
                return;
            }
            read_request(&mut stream);
            respond(&mut stream, status, &[], &body, chunked);
        })
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whether the connection opens with a TLS handshake, which a plain server just closes on.
fn is_tls(stream: &TcpStream) -> bool {
    let mut first = [0_u8; 1];
    stream.peek(&mut first).unwrap_or(0) == 1 && first[0] == 0x16
}

/// Reads one request: its head, then the body its `Content-Length` names.
fn read_request(stream: &mut (impl Read + Write)) -> String {
    let mut seen = Vec::new();
    let mut byte = [0_u8; 1];
    while !seen.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        seen.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&seen).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    let _ = stream.read_exact(&mut body);
    head
}

fn respond(
    stream: &mut impl Write,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    chunked: bool,
) {
    let mut head =
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let mut out = Vec::new();
    if chunked {
        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
        out.extend(head.into_bytes());
        for chunk in body.chunks(4096) {
            out.extend(format!("{:x}\r\n", chunk.len()).into_bytes());
            out.extend(chunk);
            out.extend(b"\r\n");
        }
        out.extend(b"0\r\n\r\n");
    } else {
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        out.extend(head.into_bytes());
        out.extend(body);
    }
    let _ = stream.write_all(&out).and_then(|()| stream.flush());
}

/// A server that answers HTTPS only, as agy's gRPC port does: a plain request gets the 400 a Go
/// server gives, and a TLS client gets `body` under a certificate made here.
fn tls_only(body: Vec<u8>) -> Server {
    use rustls::pki_types::PrivatePkcs8KeyDer;
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap(),
    );
    Server::start(move |stream, _| {
        let mut first = [0_u8; 1];
        if stream.peek(&mut first).unwrap_or(0) == 0 {
            return;
        }
        if first[0] != 0x16 {
            let mut stream = stream;
            let _ = read_request(&mut stream);
            let _ = stream.write_all(
                b"HTTP/1.0 400 Bad Request\r\n\r\nClient sent an HTTP request to an HTTPS server.\n",
            );
            return;
        }
        let Ok(connection) = rustls::ServerConnection::new(Arc::clone(&config)) else {
            return;
        };
        let mut tls = rustls::StreamOwned::new(connection, stream);
        read_request(&mut tls);
        respond(&mut tls, "200 OK", &[], &body, true);
        tls.conn.send_close_notify();
        let _ = tls.flush();
    })
}

fn find<'a>(buckets: &'a [Bucket], id: &str) -> &'a Bucket {
    buckets
        .iter()
        .find(|bucket| bucket.id == id)
        .unwrap_or_else(|| panic!("no bucket {id} in {buckets:?}"))
}

// ---------------------------------------------------------------------------------------------

#[test]
fn the_recorded_answer_over_chunked_http_is_two_unused_buckets() {
    let _only = one_at_a_time();
    let server = Server::plain("200 OK", answer(|_| {}), true);
    let mut quota = AgyQuota::new(ReadLimits::default());
    let before = unix_now();
    let buckets = quota.read(&me(), &acp_models()).unwrap();
    assert_eq!(server.accepted(), 1, "plain HTTP answered the first time");
    assert_eq!(buckets.len(), 2);

    let week = find(&buckets, "agy:gemini-4-argon-medium");
    let five = find(&buckets, "agy:gemini-3.8-flash-high");
    for (bucket, length, label) in [(week, SEVEN_DAYS, "7 d"), (five, FIVE_HOURS, "5 h")] {
        assert_eq!(bucket.source, Source::Reported);
        assert_eq!(bucket.unit, Unit::Permille);
        assert_eq!(
            (bucket.limit, bucket.remaining, bucket.used),
            (Some(1000), Some(1000), None)
        );
        assert_eq!(bucket.status, Status::Ok);
        assert_eq!(bucket.label.as_deref(), Some(label));
        assert_eq!(
            bucket.refill,
            Refill::FixedWindow {
                length: Some(Duration::from_secs(length.unsigned_abs())),
                resets_at: None,
            },
            "an unused allowance has no window yet"
        );
        assert!(bucket.observed_at >= before);
    }
    assert_eq!(
        week.models,
        Coverage::Only {
            models: vec![
                "gemini-4-argon-medium".into(),
                "gemini-4-argon-high".into(),
                "gemini-4-argon-low".into()
            ]
        }
    );
    assert_eq!(quota.unmatched(), 0);
}

#[test]
fn the_same_answer_over_tls_with_a_certificate_nobody_trusts_is_read_after_a_plain_refusal() {
    let _only = one_at_a_time();
    let server = tls_only(answer(|_| {}));
    let mut quota = AgyQuota::new(ReadLimits::default());
    assert_eq!(quota.read(&me(), &acp_models()).unwrap().len(), 2);
    assert_eq!(server.accepted(), 2, "plain was tried, then TLS");
    assert_eq!(quota.read(&me(), &acp_models()).unwrap().len(), 2);
    assert_eq!(
        server.accepted(),
        3,
        "the scheme that answered is remembered"
    );
}

#[test]
fn a_used_window_keeps_its_reset_and_the_length_seen_when_it_was_unused() {
    let _only = one_at_a_time();
    let unused = Server::plain("200 OK", answer(|_| {}), true);
    let mut quota = AgyQuota::new(ReadLimits::default());
    quota.read(&me(), &acp_models()).unwrap();
    drop(unused);

    let reset = unix_now() + 3 * 3600;
    let used = Server::plain(
        "200 OK",
        answer(|models| {
            for model in models.iter_mut() {
                if !model["label"]
                    .as_str()
                    .unwrap()
                    .starts_with("Gemini 4 Argon")
                {
                    model["quotaInfo"]["remainingFraction"] = json!(0.2504);
                    model["quotaInfo"]["resetTime"] = json!(rfc3339(reset));
                }
            }
            models[3]["quotaInfo"]["remainingFraction"] = json!(0.6);
        }),
        false,
    );
    let buckets = quota.read(&me(), &acp_models()).unwrap();
    drop(used);
    let five = find(&buckets, "agy:gemini-3.8-flash-high");
    assert_eq!(
        (five.remaining, five.used),
        (Some(250), None),
        "the least in the group"
    );
    assert_eq!(
        five.refill,
        Refill::FixedWindow {
            length: Some(Duration::from_secs(18_000)),
            resets_at: Some(reset),
        }
    );
    assert_eq!(five.label.as_deref(), Some("5 h"));
    let week = find(&buckets, "agy:gemini-4-argon-medium");
    assert_eq!(week.remaining, Some(1000));
}

#[test]
fn an_exhausted_model_exhausts_its_bucket() {
    let _only = one_at_a_time();
    let _server = Server::plain(
        "200 OK",
        answer(|models| models[0]["quotaInfo"]["remainingFraction"] = json!(0)),
        true,
    );
    let buckets = AgyQuota::new(ReadLimits::default())
        .read(&me(), &acp_models())
        .unwrap();
    let week = find(&buckets, "agy:gemini-4-argon-medium");
    assert_eq!(
        (week.status.clone(), week.remaining),
        (Status::Exhausted, Some(0))
    );
    assert_eq!(
        find(&buckets, "agy:gemini-3.8-flash-high").status,
        Status::Ok
    );
}

#[test]
fn a_model_the_agent_does_not_list_leaves_its_buckets_coverage_unknown() {
    let _only = one_at_a_time();
    let _server = Server::plain(
        "200 OK",
        answer(|models| {
            models[3]["modelId"] = json!("brand-new-model");
            models[3]["label"] = json!("Brand New");
        }),
        true,
    );
    let mut quota = AgyQuota::new(ReadLimits::default());
    let buckets = quota.read(&me(), &acp_models()).unwrap();
    let five = find(&buckets, "agy:brand-new-model");
    assert_eq!(five.models, Coverage::Unknown);
    assert_eq!(
        find(&buckets, "agy:gemini-4-argon-medium").models,
        Coverage::Only {
            models: vec![
                "gemini-4-argon-medium".into(),
                "gemini-4-argon-high".into(),
                "gemini-4-argon-low".into()
            ]
        }
    );
    assert_eq!(quota.unmatched(), 1);
}

#[test]
fn a_model_with_a_figure_that_is_not_a_number_in_range_is_left_out() {
    let _only = one_at_a_time();
    let _server = Server::plain(
        "200 OK",
        answer(|models| {
            models[0]["quotaInfo"]["remainingFraction"] = json!("most");
            models[1]["quotaInfo"]["remainingFraction"] = json!(1.5);
            models[2]["quotaInfo"]["remainingFraction"] = json!(-0.1);
            models[3]["quotaInfo"]["resetTime"] = json!("tomorrow");
            models[4]["quotaInfo"] = json!(null);
        }),
        true,
    );
    let buckets = AgyQuota::new(ReadLimits::default())
        .read(&me(), &acp_models())
        .unwrap();
    assert_eq!(
        buckets.len(),
        1,
        "nothing is left of the week, and the rest share five hours"
    );
    assert_eq!(buckets[0].id, "agy:gemini-3.8-flash-low");
}

#[test]
fn a_body_past_the_limit_is_malformed_whatever_it_holds() {
    let _only = one_at_a_time();
    let mut body: Value = serde_json::from_slice(&answer(|_| {})).unwrap();
    body["padding"] = json!("x".repeat(300 * 1024));
    let _server = Server::plain("200 OK", serde_json::to_vec(&body).unwrap(), true);
    let mut quota = AgyQuota::new(ReadLimits::default());
    assert_eq!(quota.read(&me(), &acp_models()), Err(ReadError::Malformed));
    let mut roomy = AgyQuota::new(ReadLimits::default().body(1024 * 1024));
    assert_eq!(roomy.read(&me(), &acp_models()).unwrap().len(), 2);
}

#[test]
fn an_answer_that_is_not_the_status_is_malformed_and_the_error_holds_none_of_it() {
    let _only = one_at_a_time();
    for body in [
        &br#"{"userStatus":{"email":"someone@example.com"},"name":"Someone"}"#[..],
        b"<html>hello</html>",
        b"",
        br#"{"userStatus":{"cascadeModelConfigData":{"clientModelConfigs":"someone@example.com"}}}"#,
    ] {
        let _server = Server::plain("200 OK", body.to_vec(), true);
        let error = AgyQuota::new(ReadLimits::default())
            .read(&me(), &acp_models())
            .unwrap_err();
        assert_eq!(error, ReadError::Malformed);
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains("example") && !shown.contains("Someone"), "{shown}");
    }
}

#[test]
fn a_server_that_never_answers_ends_the_reading_at_the_limit() {
    let _only = one_at_a_time();
    let _server = Server::start(|stream, stop| {
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(20));
        }
        drop(stream);
    });
    let limits = ReadLimits::default()
        .total(Duration::from_millis(600))
        .connect(Duration::from_millis(300));
    let started = Instant::now();
    let error = AgyQuota::new(limits)
        .read(&me(), &acp_models())
        .unwrap_err();
    assert_eq!(error, ReadError::Io(std::io::ErrorKind::TimedOut));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_signed_out_status_is_reported_as_it_is() {
    let _only = one_at_a_time();
    let _server = Server::plain(
        "401 Unauthorized",
        br#"{"code":"unauthenticated"}"#.to_vec(),
        false,
    );
    assert_eq!(
        AgyQuota::new(ReadLimits::default()).read(&me(), &acp_models()),
        Err(ReadError::Status(401))
    );
}

#[test]
fn a_redirect_is_not_followed() {
    let _only = one_at_a_time();
    // A port nothing listens on: following the redirect would fail to connect, not answer 302.
    let nowhere = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let location = format!("http://{nowhere}/anywhere");
    let _redirecting = Server::start(move |mut stream, _| {
        if is_tls(&stream) {
            return;
        }
        read_request(&mut stream);
        respond(
            &mut stream,
            "302 Found",
            &[("Location", &location)],
            b"",
            false,
        );
    });
    assert_eq!(
        AgyQuota::new(ReadLimits::default()).read(&me(), &acp_models()),
        Err(ReadError::Status(302))
    );
}

#[test]
fn a_reading_with_no_port_of_the_processes_is_none_and_a_port_that_moved_away_is_not_theirs() {
    let _only = one_at_a_time();
    let mut quota = AgyQuota::new(ReadLimits::default());
    assert_eq!(quota.read(&me(), &[]), Err(ReadError::NoPort));
    assert_eq!(quota.read(&[], &[]), Err(ReadError::NoPort));

    let server = Server::plain("200 OK", answer(|_| {}), true);
    assert!(quota.read(&me(), &acp_models()).is_ok());
    assert_eq!(
        quota.read(&[u32::MAX - 1], &acp_models()),
        Err(ReadError::NotOwned),
        "the port is no longer among those of the processes asked about"
    );
    drop(server);
}

#[test]
fn a_reading_soon_after_the_last_is_answered_from_it() {
    let _only = one_at_a_time();
    let server = Server::plain("200 OK", answer(|_| {}), true);
    let mut quota = AgyQuota::new(ReadLimits::default().min_interval(Duration::from_secs(60)));
    let first = quota.read(&me(), &acp_models()).unwrap();
    assert_eq!(quota.read(&me(), &acp_models()).unwrap(), first);
    assert_eq!(server.accepted(), 1);
}

#[test]
fn a_listener_this_process_owns_is_found_by_the_system() {
    let _only = one_at_a_time();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let found = loopback_listeners(&me()).unwrap();
    assert!(found.contains(&address), "{address} not in {found:?}");
    assert!(loopback_listeners(&[]).unwrap().is_empty());
    assert!(
        !loopback_listeners(&[u32::MAX - 1])
            .unwrap()
            .contains(&address),
        "another process's id does not own it"
    );
    drop(listener);
    // A socket that only connects, or listens on every interface, is not a loopback listener.
    let open = TcpListener::bind("0.0.0.0:0").unwrap();
    let port = open.local_addr().unwrap().port();
    assert!(
        loopback_listeners(&me())
            .unwrap()
            .iter()
            .all(|found| found.port() != port)
    );
}
