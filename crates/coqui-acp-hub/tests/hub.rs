//! The hub against a stand-in agent and several clients, over plain message lists.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use coqui_acp_hub::{
    Action, Answerer, Client, ClientId, Cursor, Hub, LogSnapshot, META_KEY, Note, Output, Policy,
    SnapshotError, Verdict,
};
use serde_json::{Value, json};

const SESSION: &str = "sess-1";

/// A hub, the messages each client has received, what the agent has been sent, and the host's notes.
struct Rig {
    hub: Hub,
    inbox: BTreeMap<String, Vec<Value>>,
    to_agent: Vec<Value>,
    notes: Vec<Note>,
}

impl Rig {
    fn new(hub: Hub) -> Self {
        Self {
            hub,
            inbox: BTreeMap::new(),
            to_agent: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn apply(&mut self, outputs: Vec<Output>) {
        for output in outputs {
            match output {
                Output::ToClient { client, message } => self
                    .inbox
                    .entry(client.to_string())
                    .or_default()
                    .push(message),
                Output::ToAgent(message) => self.to_agent.push(message),
                Output::Note(note) => self.notes.push(note),
                other => panic!("unexpected output {other:?}"),
            }
        }
    }

    fn connect(&mut self, client: Client) {
        self.hub.connect(client).expect("connect");
    }

    fn send(&mut self, client: &str, message: Value) {
        let outputs = self
            .hub
            .from_client(&ClientId::from(client), &message)
            .expect("from_client");
        self.apply(outputs);
    }

    fn agent(&mut self, message: Value) {
        let outputs = self.hub.from_agent(&message);
        self.apply(outputs);
    }

    fn attach(&mut self, client: &str, cursor: Option<&str>) {
        self.connect(Client::new(client));
        self.load(client, cursor);
    }

    fn load(&mut self, client: &str, cursor: Option<&str>) {
        let meta = cursor.map_or_else(|| json!({}), |c| json!({ META_KEY: { "cursor": c } }));
        self.send(
            client,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "session/load",
                    "params": { "sessionId": SESSION, "cwd": "/w", "mcpServers": [], "_meta": meta } }),
        );
    }

    fn take(&mut self, client: &str) -> Vec<Value> {
        self.inbox.remove(client).unwrap_or_default()
    }

    /// What the stand-in agent does with what it was sent: answers each prompt with one text chunk and `end_turn`.
    fn run_agent(&mut self) {
        for message in core::mem::take(&mut self.to_agent) {
            if message["method"] == "session/prompt" {
                let text = message["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                self.agent(update(&format!("re: {text}")));
                self.agent(json!({ "jsonrpc": "2.0", "id": message["id"], "result": { "stopReason": "end_turn" } }));
            }
        }
    }
}

fn update(text: &str) -> Value {
    json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": SESSION,
        "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } } } })
}

fn prompt(id: u64, text: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": { "sessionId": SESSION, "prompt": [{ "type": "text", "text": text }] } })
}

fn hub() -> Hub {
    Hub::builder(SESSION).build()
}

fn seq(message: &Value) -> u64 {
    message["params"]["_meta"][META_KEY]["seq"]
        .as_str()
        .expect("seq")
        .parse()
        .expect("number")
}

fn updates(messages: &[Value]) -> Vec<&Value> {
    messages
        .iter()
        .filter(|m| m.get("method").is_some() && m["params"]["_meta"][META_KEY]["seq"].is_string())
        .collect()
}

fn text_of(message: &Value) -> String {
    message["params"]["update"]["content"]["text"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

#[test]
fn replay_from_start_and_from_a_cursor_keep_one_order() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.send("a", prompt(2, "one"));
    rig.run_agent();
    rig.send("a", prompt(3, "two"));
    rig.run_agent();
    let live = rig.take("a");
    let live_seqs: Vec<u64> = updates(&live).iter().map(|m| seq(m)).collect();
    assert!(
        live_seqs.windows(2).all(|w| w[0] < w[1]),
        "monotonic: {live_seqs:?}"
    );

    rig.attach("b", None);
    let from_start = rig.take("b");
    let replayed = updates(&from_start);
    // user echoes, agent text and turn endings, each once, in the order they happened
    let kinds: Vec<String> = replayed
        .iter()
        .map(|m| m["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds.iter().filter(|k| *k == "session/update").count(), 4);
    assert_eq!(
        replayed.iter().map(|m| seq(m)).collect::<Vec<_>>(),
        (1..=replayed.len() as u64).collect::<Vec<_>>()
    );
    assert_eq!(text_of(replayed[0]), "one");
    assert_eq!(text_of(replayed[1]), "re: one");

    // the load's answer says where the log ends
    let answer = from_start.iter().find(|m| m["id"] == 1).expect("answer");
    assert_eq!(
        answer["result"]["_meta"][META_KEY]["cursor"],
        replayed.len().to_string()
    );
    assert_eq!(answer["result"]["_meta"][META_KEY]["truncated"], false);

    // from a cursor: exactly what follows it, identical to the tail of the full replay
    rig.attach("c", Some("2"));
    let from_cursor = rig.take("c");
    let tail: Vec<&Value> = replayed.iter().copied().filter(|m| seq(m) > 2).collect();
    assert_eq!(updates(&from_cursor), tail);

    // resume without a cursor is live only
    rig.connect(Client::new("d"));
    rig.send(
        "d",
        json!({ "jsonrpc": "2.0", "id": 9, "method": "session/resume", "params": { "sessionId": SESSION, "cwd": "/w" } }),
    );
    assert!(updates(&rig.take("d")).is_empty());
    assert_eq!(rig.hub.head(), "6".parse::<Cursor>().unwrap());
}

#[test]
fn a_gap_in_a_bounded_log_is_said() {
    let mut rig = Rig::new(Hub::builder(SESSION).max_log_entries(2).build());
    rig.attach("a", None);
    for n in 0..5 {
        rig.agent(update(&format!("t{n}")));
    }
    assert_eq!(rig.hub.log_len(), 2);
    rig.attach("b", None);
    let got = rig.take("b");
    assert_eq!(updates(&got).len(), 2);
    let answer = got.iter().find(|m| m["id"] == 1).unwrap();
    assert_eq!(answer["result"]["_meta"][META_KEY]["truncated"], true);
}

#[test]
fn updates_reach_every_attached_client_and_none_before_attach() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.attach("b", None);
    rig.connect(Client::new("lurker"));
    rig.take("a");
    rig.take("b");
    rig.agent(update("hello"));
    assert_eq!(text_of(&rig.take("a")[0]), "hello");
    assert_eq!(text_of(&rig.take("b")[0]), "hello");
    assert!(rig.take("lurker").is_empty());
    assert_eq!(rig.hub.attached_count(), 2);
}

#[test]
fn prompts_from_two_clients_queue_in_order_and_are_stamped() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.attach("b", None);
    rig.take("a");
    rig.take("b");
    rig.send("a", prompt(10, "first"));
    rig.send("b", prompt(20, "second"));
    // only the first is with the agent; the second waits
    assert_eq!(rig.to_agent.len(), 1);
    assert_eq!(rig.to_agent[0]["params"]["_meta"][META_KEY]["client"], "a");
    assert_eq!(rig.to_agent[0]["params"]["_meta"][META_KEY]["prompt"], 1);
    assert_eq!(rig.hub.queued_prompts(), 1);

    // each client sees the other's prompt, stamped with who sent it, and not its own
    let seen_by_b = rig.take("b");
    let echo = updates(&seen_by_b)
        .into_iter()
        .find(|m| m["params"]["update"]["sessionUpdate"] == "user_message_chunk")
        .unwrap();
    assert_eq!(echo["params"]["_meta"][META_KEY]["client"], "a");
    assert_eq!(text_of(echo), "first");
    let seen_by_a = rig.take("a");
    let echoes: Vec<_> = updates(&seen_by_a).into_iter().map(text_of).collect();
    assert_eq!(echoes, ["second"]);

    rig.run_agent(); // answers "first", which starts "second"
    assert_eq!(rig.to_agent.len(), 1);
    assert_eq!(rig.to_agent[0]["params"]["_meta"][META_KEY]["client"], "b");
    assert_eq!(rig.to_agent[0]["params"]["_meta"][META_KEY]["prompt"], 2);
    rig.run_agent();

    // each sender got its own answer under its own request id
    let a = rig.take("a");
    let b = rig.take("b");
    assert!(
        a.iter()
            .any(|m| m["id"] == 10 && m["result"]["stopReason"] == "end_turn")
    );
    assert!(
        b.iter()
            .any(|m| m["id"] == 20 && m["result"]["stopReason"] == "end_turn")
    );
    assert!(!a.iter().any(|m| m["id"] == 20) && !b.iter().any(|m| m["id"] == 10));
    // the others learn a turn ended
    assert!(
        b.iter()
            .any(|m| m["method"] == coqui_acp_hub::TURN_ENDED && m["params"]["client"] == "a")
    );
}

#[test]
fn a_full_queue_refuses_the_prompt_to_its_sender() {
    let mut rig = Rig::new(Hub::builder(SESSION).max_queued_prompts(1).build());
    rig.attach("a", None);
    rig.send("a", prompt(2, "running"));
    rig.send("a", prompt(3, "queued"));
    rig.send("a", prompt(4, "too many"));
    let got = rig.take("a");
    assert!(got.iter().any(|m| m["id"] == 4 && m["error"].is_object()));
    assert!(matches!(rig.notes.last(), Some(Note::Refused { action, .. }) if action == "prompt"));
}

fn permission(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "session/request_permission", "params": {
        "sessionId": SESSION, "toolCall": { "toolCallId": "t1" },
        "options": [{ "optionId": "allow", "name": "Allow", "kind": "allow_once" }, { "optionId": "deny", "name": "Deny", "kind": "reject_once" }] } })
}

fn selected(id: &Value, option: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "selected", "optionId": option } } })
}

fn permission_id(messages: &[Value]) -> Value {
    messages
        .iter()
        .find(|m| m["method"] == "session/request_permission")
        .expect("a request")["id"]
        .clone()
}

#[test]
fn a_permission_is_answered_by_one_client_and_closed_for_the_others() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.attach("b", None);
    rig.take("a");
    rig.take("b");
    rig.agent(permission(77));
    let id_a = permission_id(&rig.take("a"));
    let id_b = permission_id(&rig.take("b"));
    assert_eq!(rig.hub.pending_permissions().count(), 1);

    // an option the request did not offer is not an answer
    rig.send("b", selected(&id_b, "bogus"));
    assert!(rig.to_agent.is_empty());

    rig.send("b", selected(&id_b, "allow"));
    assert_eq!(rig.to_agent.len(), 1);
    assert_eq!(rig.to_agent[0]["id"], 77);
    assert_eq!(rig.to_agent[0]["result"]["outcome"]["optionId"], "allow");
    assert!(rig.notes.iter().any(|n| matches!(n, Note::PermissionAnswered { by: Answerer::Client(c), .. } if c.as_str() == "b")));
    let closed = rig.take("a");
    let closed = closed
        .iter()
        .find(|m| m["method"] == coqui_acp_hub::PERMISSION_CLOSED)
        .expect("told");
    assert_eq!(closed["params"]["by"], "b");
    assert_eq!(rig.hub.pending_permissions().count(), 0);

    // the slower client's answer changes nothing
    rig.send("a", selected(&id_a, "deny"));
    assert_eq!(rig.to_agent.len(), 1);
    assert!(matches!(rig.notes.last(), Some(Note::Ignored { .. })));
}

#[test]
fn a_client_that_attaches_later_is_asked_what_is_still_open() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.agent(permission(5));
    rig.attach("b", None);
    let id = permission_id(&rig.take("b"));
    rig.send("b", selected(&id, "deny"));
    assert_eq!(rig.to_agent[0]["id"], 5);
}

#[test]
fn a_permission_waits_when_nobody_is_attached_and_the_host_hears_of_it() {
    let mut rig = Rig::new(
        Hub::builder(SESSION)
            .permission_window_ms(Some(1000))
            .build(),
    );
    rig.poll(0);
    rig.agent(permission(5));
    rig.poll(999);
    assert!(rig.notes.is_empty());
    rig.poll(1500);
    assert!(matches!(
        rig.notes.as_slice(),
        [Note::PermissionUnanswered {
            waiting_ms: 1500,
            ..
        }]
    ));
    rig.poll(3000);
    assert_eq!(rig.notes.len(), 1, "reported once");
    // never answered on its own
    assert!(rig.to_agent.is_empty());
    assert_eq!(rig.hub.pending_permissions().count(), 1);

    // the host answers it
    let request = rig.hub.pending_permissions().next().unwrap().id;
    assert!(
        rig.hub
            .decide(
                request,
                json!({ "outcome": { "outcome": "selected", "optionId": "bogus" } })
            )
            .is_err()
    );
    let outputs = rig
        .hub
        .decide(
            request,
            json!({ "outcome": { "outcome": "selected", "optionId": "deny" } }),
        )
        .unwrap();
    rig.apply(outputs);
    assert_eq!(rig.to_agent[0]["id"], 5);
    assert!(rig.notes.iter().any(|n| matches!(
        n,
        Note::PermissionAnswered {
            by: Answerer::Host,
            ..
        }
    )));
}

impl Rig {
    fn poll(&mut self, now: u64) {
        let outputs = self.hub.poll(now);
        self.apply(outputs);
    }
}

#[test]
fn when_every_client_asked_declines_the_agent_is_told_cancelled() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.attach("b", None);
    rig.take("a");
    rig.take("b");
    rig.agent(permission(8));
    let id_a = permission_id(&rig.take("a"));
    let id_b = permission_id(&rig.take("b"));
    let cancelled = |id: &Value| json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "cancelled" } } });
    rig.send("a", cancelled(&id_a));
    assert!(rig.to_agent.is_empty(), "b may still answer");
    rig.send("b", cancelled(&id_b));
    assert_eq!(rig.to_agent[0]["result"]["outcome"]["outcome"], "cancelled");
    assert!(rig.notes.iter().any(|n| matches!(
        n,
        Note::PermissionAnswered {
            by: Answerer::Declined,
            ..
        }
    )));
}

#[test]
fn a_detach_leaves_the_session_running_and_a_client_can_come_back() {
    let mut rig = Rig::new(hub());
    rig.poll(10);
    rig.attach("a", None);
    rig.send("a", prompt(2, "work"));
    rig.hub.disconnect(&ClientId::from("a")).unwrap();
    assert_eq!(rig.hub.attached_count(), 0);
    assert!(rig.hub.turn_running());
    rig.poll(50);
    rig.run_agent(); // the turn finishes with nobody there
    assert!(!rig.hub.turn_running());
    assert!(!rig.hub.ended());
    assert_eq!(rig.hub.last_activity(), 50);
    rig.take("a");

    // another device attaches and sees what happened
    rig.attach("phone", None);
    let got = rig.take("phone");
    let texts: Vec<String> = updates(&got)
        .into_iter()
        .filter(|m| m["method"] == "session/update")
        .map(text_of)
        .collect();
    assert_eq!(texts, ["work", "re: work"]);
    // and prompts into the same session
    rig.send("phone", prompt(3, "more"));
    assert_eq!(rig.to_agent.len(), 1);
}

#[test]
fn a_cancel_from_any_client_reaches_the_agent_once_a_turn_runs() {
    let mut rig = Rig::new(Hub::builder(SESSION).cancel_drops_queue(true).build());
    rig.attach("a", None);
    rig.attach("b", None);
    rig.send("a", prompt(2, "long"));
    rig.send("b", prompt(3, "next"));
    rig.to_agent.clear();
    rig.send(
        "b",
        json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": SESSION } }),
    );
    assert_eq!(rig.to_agent[0]["method"], "session/cancel");
    assert!(
        rig.take("b")
            .iter()
            .any(|m| m["id"] == 3 && m["result"]["stopReason"] == "cancelled")
    );
    assert_eq!(rig.hub.queued_prompts(), 0);
}

struct Roles;
impl Policy for Roles {
    fn check(&self, client: &Client, action: &Action<'_>) -> Verdict {
        match (client.role.as_str(), action) {
            ("banned", _) => Verdict::Deny("banned".into()),
            ("viewer", Action::Prompt | Action::Cancel | Action::Answer { .. }) => {
                Verdict::Deny("read only".into())
            }
            _ => Verdict::Allow,
        }
    }
}

#[test]
fn a_policy_refuses_a_client_and_limits_another() {
    let mut rig = Rig::new(Hub::builder(SESSION).policy(Arc::new(Roles)).build());
    let refused = rig
        .hub
        .connect(Client::new("x").role("banned"))
        .unwrap_err();
    assert_eq!(refused, coqui_acp_hub::HubError::Refused("banned".into()));
    assert_eq!(
        rig.hub.connect(Client::new("a").subject("person-7")),
        Ok(())
    );
    assert!(
        rig.hub.connect(Client::new("a")).is_err(),
        "duplicate identity"
    );

    rig.load("a", None);
    rig.connect(Client::new("v").role("viewer"));
    rig.load("v", None);
    rig.take("a");
    // The subject is the host's mapping of a client to a person: the other clients see who is there, never that.
    let seen = rig.take("v");
    assert!(seen.iter().any(|m| m["method"] == coqui_acp_hub::PRESENCE));
    assert!(
        !seen.iter().any(|m| m.to_string().contains("person-7")),
        "{seen:?}"
    );

    rig.send("v", prompt(2, "let me in"));
    assert!(
        rig.take("v")
            .iter()
            .any(|m| m["id"] == 2 && m["error"]["message"] == "read only")
    );
    assert!(rig.to_agent.is_empty());
    assert!(
        matches!(rig.notes.last(), Some(Note::Refused { client, .. }) if client.as_str() == "v")
    );

    // a viewer watches but is never asked
    rig.agent(permission(4));
    assert!(
        rig.take("v")
            .iter()
            .all(|m| m["method"] != "session/request_permission")
    );
    assert!(!rig.take("a").is_empty());
    rig.send("v", selected(&json!("hub-perm-1"), "allow"));
    assert!(rig.to_agent.is_empty());

    // presence carries who is attached, as the host named them
    let presence = |m: &Value| m["method"] == coqui_acp_hub::PRESENCE;
    rig.attach("w", None);
    let got = rig.take("a");
    let p = got.iter().rev().find(|m| presence(m)).unwrap();
    assert_eq!(p["params"]["clients"].as_array().unwrap().len(), 3);
}

#[test]
fn a_client_must_attach_before_it_prompts_and_the_session_must_match() {
    let mut rig = Rig::new(hub());
    rig.connect(Client::new("a"));
    rig.send("a", prompt(1, "early"));
    assert!(rig.take("a")[0]["error"].is_object());
    rig.send("a", json!({ "jsonrpc": "2.0", "id": 2, "method": "session/load", "params": { "sessionId": "other", "cwd": "/" } }));
    assert!(rig.take("a")[0]["error"].is_object());
    rig.send(
        "a",
        json!({ "jsonrpc": "2.0", "id": 3, "method": "session/new", "params": { "cwd": "/" } }),
    );
    assert!(rig.take("a")[0]["error"].is_object());
    assert_eq!(rig.hub.attached_count(), 0);
}

#[test]
fn initialize_is_answered_from_the_agents_with_load_and_resume_added() {
    let hub = Hub::builder(SESSION)
        .initialize_result(json!({ "protocolVersion": 1, "agentCapabilities": { "promptCapabilities": { "image": true } } }))
        .build();
    let mut rig = Rig::new(hub);
    rig.connect(Client::new("a"));
    rig.send("a", json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": 1 } }));
    let got = rig.take("a");
    let caps = &got[0]["result"]["agentCapabilities"];
    assert_eq!(caps["loadSession"], true);
    assert!(caps["sessionCapabilities"]["resume"].is_object());
    assert_eq!(caps["promptCapabilities"]["image"], true);
}

#[test]
fn other_requests_pass_through_and_the_answer_comes_back_to_the_asker() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.take("a");
    rig.send(
        "a",
        json!({ "jsonrpc": "2.0", "id": 41, "method": "session/set_mode", "params": { "sessionId": SESSION, "modeId": "plan" } }),
    );
    let forwarded = rig.to_agent.pop().unwrap();
    assert_eq!(forwarded["method"], "session/set_mode");
    rig.agent(json!({ "jsonrpc": "2.0", "id": forwarded["id"], "result": {} }));
    assert!(
        rig.take("a")
            .iter()
            .any(|m| m["id"] == 41 && m["result"].is_object())
    );
}

#[test]
fn the_agent_ending_fails_what_was_waiting_and_ends_the_hub() {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.send("a", prompt(2, "x"));
    rig.send("a", prompt(3, "y"));
    rig.take("a");
    let outputs = rig.hub.agent_closed();
    rig.apply(outputs);
    let got = rig.take("a");
    assert!(got.iter().any(|m| m["id"] == 2 && m["error"].is_object()));
    assert!(got.iter().any(|m| m["id"] == 3 && m["error"].is_object()));
    assert!(got.iter().any(|m| m["method"] == coqui_acp_hub::ENDED));
    assert!(rig.hub.ended());
    rig.send("a", prompt(4, "z"));
    assert!(rig.take("a")[0]["error"].is_object());
}

#[test]
fn settings_report_what_is_not_the_default() {
    let defaults = coqui_acp_hub::Settings::default();
    assert!(defaults.non_defaults().is_empty());
    let hub = Hub::builder(SESSION)
        .max_queued_prompts(0)
        .max_log_entries(10)
        .build();
    let names: Vec<_> = hub
        .settings()
        .non_defaults()
        .into_iter()
        .map(|n| n.setting)
        .collect();
    assert_eq!(names, ["max_log_entries", "max_queued_prompts"]);
    assert_eq!("12".parse::<Cursor>().unwrap().to_string(), "12");
    assert!("x".parse::<Cursor>().is_err());
}

/// A hub that has run two turns, a client having seen the first of them.
fn busy_rig() -> Rig {
    let mut rig = Rig::new(hub());
    rig.attach("a", None);
    rig.send("a", prompt(2, "one"));
    rig.run_agent();
    rig.send("a", prompt(3, "two"));
    rig.run_agent();
    rig
}

/// A hub for the same session built from what `rig`'s hub kept, through the text a host would write to disk.
fn restarted(rig: &Rig, builder: coqui_acp_hub::Builder) -> Rig {
    let text = rig.hub.log_snapshot().to_json().to_string();
    let snapshot = LogSnapshot::from_json(&serde_json::from_str(&text).unwrap()).unwrap();
    Rig::new(builder.restore_log(snapshot).expect("restore").build())
}

fn load_error(messages: &[Value]) -> &Value {
    &messages.iter().find(|m| m["id"] == 1).expect("answer")["error"]
}

#[test]
fn a_restarted_hub_serves_the_cursor_a_client_held_in_the_same_order() {
    let mut rig = busy_rig();
    // a client that stopped at the end of the first turn holds this cursor (the echo, the text, the turn's end)
    rig.attach("phone", Some("3"));
    let before = rig.take("phone");
    let before_tail: Vec<Value> = updates(&before).into_iter().cloned().collect();
    assert_eq!(before_tail.iter().map(seq).collect::<Vec<_>>(), [4, 5, 6]);
    let head = rig.hub.head();

    let mut again = restarted(&rig, Hub::builder(SESSION));
    assert_eq!(again.hub.head(), head);
    assert_eq!(again.hub.log_len(), rig.hub.log_len());

    // the client comes back to the new hub with the cursor it held
    again.attach("phone", Some("3"));
    let after = again.take("phone");
    assert_eq!(
        updates(&after).into_iter().cloned().collect::<Vec<_>>(),
        before_tail
    );
    let answer = after.iter().find(|m| m["id"] == 1).unwrap();
    assert_eq!(answer["result"]["_meta"][META_KEY]["cursor"], "6");

    // a fresh client replays the whole log, as it would have before the restart
    again.attach("new", None);
    let all = again.take("new");
    assert_eq!(
        updates(&all).iter().map(|m| seq(m)).collect::<Vec<_>>(),
        (1..=6).collect::<Vec<_>>()
    );
    assert_eq!(text_of(updates(&all)[0]), "one");

    // new updates carry on from the head: cursors and prompt numbers are not issued twice
    again.send("phone", prompt(5, "three"));
    again.run_agent();
    let live = again.take("phone");
    let live_seqs: Vec<u64> = updates(&live).iter().map(|m| seq(m)).collect();
    assert_eq!(
        live_seqs,
        [8],
        "the sender is told its turn ended by the answer, not the notice"
    );
    let seen_by_new = again.take("new");
    let seen_by_new = updates(&seen_by_new);
    assert_eq!(
        seen_by_new.iter().map(|m| seq(m)).collect::<Vec<_>>(),
        [7, 8, 9]
    );
    assert_eq!(seen_by_new[0]["params"]["_meta"][META_KEY]["prompt"], 3);
}

#[test]
fn a_hub_restarted_without_its_log_refuses_a_cursor_from_the_old_one() {
    let mut rig = busy_rig();
    rig.take("a");
    let mut again = Rig::new(hub());
    again.agent(update("fresh"));
    again.connect(Client::new("phone"));
    again.load("phone", Some("6"));
    let got = again.take("phone");
    assert!(updates(&got).is_empty(), "nothing is replayed");
    let error = load_error(&got);
    assert_eq!(error["data"]["reason"], "cursor_ahead");
    assert_eq!(error["data"]["head"], "1");
    assert_eq!(
        again.hub.attached_count(),
        0,
        "a refused load attaches nothing"
    );

    // the client may start over without a cursor
    again.load("phone", None);
    assert_eq!(updates(&again.take("phone")).len(), 1);
}

#[test]
fn a_cursor_older_than_the_kept_log_is_an_error_not_a_partial_replay() {
    let mut rig = Rig::new(Hub::builder(SESSION).max_log_entries(3).build());
    rig.attach("a", None);
    for n in 0..6 {
        rig.agent(update(&format!("t{n}")));
    }
    rig.take("a");
    // kept: 4, 5, 6. A client at 3 misses nothing; one at 2 would miss 3.
    rig.attach("b", Some("3"));
    assert_eq!(updates(&rig.take("b")).len(), 3);
    rig.connect(Client::new("c"));
    rig.load("c", Some("2"));
    let got = rig.take("c");
    assert!(updates(&got).is_empty());
    let error = load_error(&got);
    assert_eq!(error["data"]["reason"], "cursor_expired");
    assert_eq!(error["data"]["oldest"], "4");
    assert_eq!(error["data"]["head"], "6");

    // the same holds after a restart whose bounds are tighter than the kept log
    let mut again = restarted(&rig, Hub::builder(SESSION).max_log_entries(2));
    assert_eq!(again.hub.log_len(), 2);
    assert_eq!(again.hub.head(), "6".parse::<Cursor>().unwrap());
    again.connect(Client::new("b"));
    again.load("b", Some("3"));
    assert_eq!(
        load_error(&again.take("b"))["data"]["reason"],
        "cursor_expired"
    );
    again.attach("d", Some("4"));
    assert_eq!(updates(&again.take("d")).len(), 2);
}

#[test]
fn a_cursor_that_is_not_one_is_refused_rather_than_replayed_from() {
    let mut rig = busy_rig();
    rig.take("a");
    rig.connect(Client::new("b"));
    rig.load("b", Some("not-a-cursor"));
    let got = rig.take("b");
    assert!(updates(&got).is_empty());
    assert_eq!(load_error(&got)["data"]["reason"], "bad_cursor");
}

#[test]
fn a_log_with_a_bad_entry_is_not_restored() {
    let mut rig = busy_rig();
    rig.take("a");
    let good = rig.hub.log_snapshot();
    let mut entries = good.entries.clone();
    entries.swap(0, 1);
    let reordered = LogSnapshot::new(good.head, entries);
    assert!(matches!(
        Hub::builder(SESSION).restore_log(reordered),
        Err(SnapshotError::OutOfOrder(1))
    ));
    let short = LogSnapshot::new("3".parse().unwrap(), good.entries.clone());
    assert!(matches!(
        Hub::builder(SESSION).restore_log(short),
        Err(SnapshotError::PastHead(3))
    ));
    let bare = LogSnapshot::new(good.head, vec![json!({ "method": "session/update" })]);
    assert!(matches!(
        Hub::builder(SESSION).restore_log(bare),
        Err(SnapshotError::NoCursor(0))
    ));
    assert!(LogSnapshot::from_json(&json!({ "head": "1" })).is_err());
}

#[test]
fn a_host_keeps_the_log_by_appending_what_followed_the_cursor_it_last_kept() {
    let mut rig = busy_rig();
    let mut kept: Vec<Value> = rig.hub.log_after(Cursor::START).cloned().collect();
    let kept_to = rig.hub.head();
    rig.send("a", prompt(4, "three"));
    rig.run_agent();
    kept.extend(rig.hub.log_after(kept_to).cloned());
    assert_eq!(kept, rig.hub.log_snapshot().entries);
    assert!(rig.hub.log_after(rig.hub.head()).next().is_none());
}
