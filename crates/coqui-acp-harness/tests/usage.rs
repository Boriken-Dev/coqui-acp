//! The usage extension served for a harness: the capability, `_coqui_acp/usage`, the notification for a
//! client that asked, revisions, and a limited turn.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use coqui_acp_api::usage::{Bucket, LimitHit, Refill, Spend, Status};
use coqui_acp_api::{Source, UsageReport, UsageSupport};
use coqui_acp_harness::event::{Event, Stop, TurnEnd, Usage};
use coqui_acp_harness::{
    AgentInfo, ConfigOption, Conversation, EventSink, Harness, HarnessError, OpenRequest, Settings,
    Takes, serve,
};
use serde_json::{Value, json};

/// What a prompt of the scripted harness reports, in order.
#[derive(Clone, Default)]
struct Script {
    buckets: Vec<Bucket>,
    spend: bool,
    limit: Option<LimitHit>,
}

#[derive(Clone)]
struct Reports {
    support: UsageSupport,
    script: Arc<Mutex<Script>>,
}

impl Reports {
    fn new(support: UsageSupport) -> Self {
        Self {
            support,
            script: Arc::default(),
        }
    }

    fn says(&self, script: Script) {
        *self.script.lock().unwrap() = script;
    }
}

struct Open {
    events: EventSink,
    script: Arc<Mutex<Script>>,
}

impl Harness for Reports {
    fn info(&self) -> AgentInfo {
        AgentInfo::new("reports", "Reports", "0")
    }

    fn takes(&self) -> Takes {
        Takes::new()
    }

    fn usage(&self) -> UsageSupport {
        self.support.clone()
    }

    fn open(
        &self,
        _request: OpenRequest,
        events: EventSink,
    ) -> Result<Box<dyn Conversation>, HarnessError> {
        Ok(Box::new(Open {
            events,
            script: Arc::clone(&self.script),
        }))
    }
}

impl Conversation for Open {
    fn session_id(&self) -> String {
        "s1".into()
    }

    fn options(&self) -> Vec<ConfigOption> {
        Vec::new()
    }

    fn prompt(&self, _content: Vec<coqui_acp_harness::event::Content>) -> Result<(), HarnessError> {
        let script = self.script.lock().unwrap().clone();
        let events = self.events.clone();
        std::thread::spawn(move || {
            if !script.buckets.is_empty() {
                events.send(Event::Buckets(script.buckets));
            }
            if script.spend {
                let mut usage = Usage::default();
                let mut spend = Spend::new("");
                spend.input = 7;
                spend.output = 3;
                usage.spend = Some(spend);
                events.send(Event::Usage(usage));
            }
            events.send(Event::TurnEnd(match script.limit {
                Some(hit) => TurnEnd::Limited(hit),
                None => TurnEnd::stopped(Stop::EndTurn),
            }));
        });
        Ok(())
    }

    fn interrupt(&self) -> Result<(), HarnessError> {
        Ok(())
    }

    fn set_option(&self, _id: &str, _value: &str) -> Result<Vec<ConfigOption>, HarnessError> {
        Ok(Vec::new())
    }

    fn close(&self) {}
}

struct Wire {
    to_agent: std::io::PipeWriter,
    from_agent: Receiver<Value>,
    next: u64,
    seen: Vec<Value>,
}

impl Wire {
    fn serve(harness: Reports, settings: Settings) -> Self {
        let (agent_reads, client_writes) = std::io::pipe().unwrap();
        let (client_reads, agent_writes) = std::io::pipe().unwrap();
        std::thread::spawn(move || serve(Arc::new(harness), settings, agent_reads, agent_writes));
        let (sender, from_agent) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(client_reads).lines() {
                let Ok(line) = line else { return };
                if sender.send(serde_json::from_str(&line).unwrap()).is_err() {
                    return;
                }
            }
        });
        Self {
            to_agent: client_writes,
            from_agent,
            next: 0,
            seen: Vec::new(),
        }
    }

    /// Sends a request and reads until it is answered; what comes between is kept in `seen`.
    fn ask(&mut self, method: &str, params: &Value) -> Value {
        self.next += 1;
        let id = self.next;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.to_agent.write_all(&line).unwrap();
        loop {
            let message = self
                .from_agent
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("{method} was not answered"));
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message;
            }
            self.seen.push(message);
        }
    }

    /// The usage notifications seen since the last call, once everything the agent sent has been read.
    fn told(&mut self) -> Vec<Value> {
        while let Ok(message) = self.from_agent.recv_timeout(Duration::from_millis(300)) {
            self.seen.push(message);
        }
        std::mem::take(&mut self.seen)
            .into_iter()
            .filter(|message| message["method"] == "_coqui_acp/usage_update")
            .collect()
    }

    fn report(&mut self, session: Option<&str>) -> UsageReport {
        let params = session.map_or_else(|| json!({}), |session| json!({"sessionId": session}));
        let answer = self.ask("_coqui_acp/usage", &params);
        serde_json::from_value(answer["result"].clone()).unwrap_or_else(|e| panic!("{e}: {answer}"))
    }
}

fn five_hour(used: u64) -> Bucket {
    let mut bucket = Bucket::new("five_hour");
    bucket.limit = Some(1000);
    bucket.used = Some(used);
    bucket.status = Status::Ok;
    bucket.refill = Refill::FixedWindow {
        length: Some(Duration::from_secs(18_000)),
        resets_at: Some(4_000_000_000),
    };
    bucket
}

fn cwd() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

fn reporting() -> UsageSupport {
    UsageSupport::new(vec![Source::Reported], true)
}

fn opted_in(wire: &mut Wire, version: u64, notify: bool) -> Value {
    let key = "dev.boriken.coqui.acp/usage";
    wire.ask(
        "initialize",
        &json!({"protocolVersion": version, "clientCapabilities": {"_meta": {key: {"notify": notify, "account": "pool/alice"}}}}),
    )
}

fn session(wire: &mut Wire) -> String {
    let answer = wire.ask("session/new", &json!({"cwd": cwd(), "mcpServers": []}));
    answer["result"]["sessionId"]
        .as_str()
        .expect("a session")
        .to_owned()
}

fn prompt(wire: &mut Wire, session: &str) -> Value {
    wire.ask(
        "session/prompt",
        &json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    )
}

#[test]
fn a_harness_that_reports_none_advertises_nothing_and_answers_method_not_found() {
    for version in [1, 2] {
        let mut wire = Wire::serve(Reports::new(UsageSupport::none()), Settings::new());
        let initialized = opted_in(&mut wire, version, true);
        assert!(
            !initialized["result"]
                .to_string()
                .contains("dev.boriken.coqui.acp/usage"),
            "{initialized}"
        );
        let answer = wire.ask("_coqui_acp/usage", &json!({}));
        assert_eq!(answer["error"]["code"], -32601, "{answer}");
    }
}

#[test]
fn a_harness_that_reports_advertises_it_in_either_versions_capabilities() {
    for (version, key) in [(1, "agentCapabilities"), (2, "capabilities")] {
        let mut wire = Wire::serve(Reports::new(reporting()), Settings::new());
        let initialized = opted_in(&mut wire, version, false);
        assert_eq!(
            initialized["result"][key]["_meta"]["dev.boriken.coqui.acp/usage"],
            json!({"version": 1, "sources": ["reported"], "spend": true}),
            "{initialized}"
        );
    }
}

#[test]
fn the_report_is_answerable_with_no_session_and_no_turn() {
    let mut wire = Wire::serve(Reports::new(reporting()), Settings::new());
    opted_in(&mut wire, 1, true);
    let report = wire.report(None);
    assert!(report.supported && report.buckets.is_empty() && report.spend.is_none());
    assert_eq!(report.account.as_deref(), Some("pool/alice"));
    assert_eq!(report.sources, vec![Source::Reported]);
    assert_eq!(
        wire.report(None),
        report,
        "an unchanged account reads the same"
    );
    let unknown = wire.ask("_coqui_acp/usage", &json!({"sessionId": "nope"}));
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
}

#[test]
fn a_change_is_told_to_the_client_that_asked_and_a_repeat_changes_nothing() {
    let harness = Reports::new(reporting());
    let mut wire = Wire::serve(harness.clone(), Settings::new());
    let mut quiet = Wire::serve(harness.clone(), Settings::new());
    opted_in(&mut wire, 1, true);
    opted_in(&mut quiet, 1, false);
    let (s, q) = (session(&mut wire), session(&mut quiet));
    harness.says(Script {
        buckets: vec![five_hour(620)],
        spend: true,
        limit: None,
    });
    assert_eq!(prompt(&mut wire, &s)["result"]["stopReason"], "end_turn");
    prompt(&mut quiet, &q);
    let told = wire.told();
    assert_eq!(told.len(), 2, "the bucket, then the spend: {told:?}");
    assert!(
        quiet.told().is_empty(),
        "a client that did not ask was told"
    );
    let last: UsageReport = serde_json::from_value(told[1]["params"]["report"].clone()).unwrap();
    assert_eq!(told[1]["params"]["sessionId"], json!(s));
    assert_eq!(
        last,
        wire.report(Some(&s)),
        "the last notification is the report"
    );
    assert_eq!(last.revision, 2);
    assert_eq!(
        last.spend
            .as_ref()
            .map(|spend| (spend.session_id.as_str(), spend.input)),
        Some((s.as_str(), 7))
    );
    assert!(wire.report(None).spend.is_none(), "no session, no spend");
    let five = &last.buckets[0];
    assert_eq!(
        (five.used, five.limit, UsageReport::fraction_permille(five)),
        (Some(620), Some(1000), Some(620))
    );
    assert_ne!(five.observed_at, 0, "the connection stamps what it hears");

    // The same figures again: no new revision, and nothing told but the unchanged spend's total.
    harness.says(Script {
        buckets: vec![five_hour(620)],
        spend: false,
        limit: None,
    });
    prompt(&mut wire, &s);
    assert!(wire.told().is_empty(), "a repeat was told");
    assert_eq!(wire.report(None).revision, 2);
    harness.says(Script {
        buckets: vec![five_hour(700)],
        spend: false,
        limit: None,
    });
    prompt(&mut wire, &s);
    assert_eq!(wire.told().len(), 1);
    assert_eq!(wire.report(None).revision, 3);
}

#[test]
fn a_limited_turn_is_a_refusal_with_its_hit_and_the_report_keeps_it() {
    let harness = Reports::new(reporting());
    let mut wire = Wire::serve(harness.clone(), Settings::new());
    opted_in(&mut wire, 1, true);
    let s = session(&mut wire);
    let mut hit = LimitHit::new(0);
    hit.bucket = Some("five_hour".into());
    hit.resets_at = Some(4_000_000_000);
    hit.retry_after = Some(Duration::from_secs(90));
    hit.text = Some("usage limit".into());
    harness.says(Script {
        buckets: vec![five_hour(990)],
        spend: false,
        limit: Some(hit.clone()),
    });
    let ended = prompt(&mut wire, &s);
    assert_eq!(ended["result"]["stopReason"], "refusal", "{ended}");
    let meta = &ended["result"]["_meta"]["dev.boriken.coqui.acp/limit"];
    let told: LimitHit = serde_json::from_value(meta.clone()).unwrap();
    assert_eq!(told.bucket, hit.bucket);
    assert_eq!(told.resets_at, hit.resets_at);
    assert_eq!(told.retry_after, Some(Duration::from_secs(90)));
    assert_ne!(told.observed_at, 0);
    let report = wire.report(None);
    assert_eq!(report.limit.as_ref(), Some(&told));
    assert_eq!(report.buckets[0].status, Status::Exhausted);
    // Room reported afterwards lifts it.
    harness.says(Script {
        buckets: vec![five_hour(10)],
        spend: false,
        limit: None,
    });
    prompt(&mut wire, &s);
    assert!(wire.report(None).limit.is_none());
}

#[test]
fn the_hosts_account_is_over_the_clients() {
    let mut settings = Settings::new();
    settings.account = Some("pool/bob".into());
    let mut wire = Wire::serve(Reports::new(reporting()), settings);
    opted_in(&mut wire, 1, false);
    assert_eq!(wire.report(None).account.as_deref(), Some("pool/bob"));
}
