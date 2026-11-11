//! The secret-prompt extension through its public API: a request and a show end to end, and what each refuses.

#![cfg(feature = "secret")]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use coqui_acp::secret::{
    BINDING_ECDSA, CAPABILITY, Capability, Error, Form, IdentityId, Limits, Method, Prover,
    Receiver, RequestParams, RequestReply, Sealed, ShowDeliver, ShowDone, ShowOpen, ShowOpened,
    Verifier,
};
use proptest::prelude::*;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

const NOW: u64 = 1_700_000_000;

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "user": {"type": "string"},
            "password": {"type": "string", "writeOnly": true, "minLength": 8},
            "note": {"type": "string", "x-sensitive": true},
            "port": {"type": "integer", "minimum": 1, "maximum": 65535}
        },
        "required": ["user", "password"]
    })
}

fn scalar() -> Vec<u8> {
    (1u8..=32).collect()
}

fn x25519(seed: u8) -> Vec<u8> {
    vec![seed; 32]
}

fn x25519_spki(secret: &[u8]) -> Vec<u8> {
    let secret: [u8; 32] = secret.try_into().unwrap();
    let mut spki = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
    ];
    spki.extend_from_slice(
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(secret)).as_bytes(),
    );
    spki
}

fn form() -> Form {
    Form::parse(&schema(), &Limits::new()).unwrap()
}

#[test]
fn a_request_reaches_the_host_sealed_and_nothing_else_holds_the_value() {
    let prover = Prover::ecdsa_p256(&scalar()).unwrap();
    let form = form();
    let receiver = Receiver::new(
        Method::Request,
        "r1",
        &form,
        &prover,
        NOW,
        Duration::from_secs(300),
    )
    .unwrap();
    let params = RequestParams {
        session_id: "s1".into(),
        request_id: "r1".into(),
        message: "The deploy needs a login".into(),
        schema: schema(),
        form: None,
        recipient: receiver.recipient().clone(),
    };
    // The host's message crosses a pipe: only JSON survives.
    let params =
        RequestParams::from_value(&serde_json::from_str(&params.to_value().to_string()).unwrap())
            .unwrap();

    let collector = Verifier::builder()
        .trust(prover.id())
        .build()
        .collector(
            Method::Request,
            &params.request_id,
            &Form::parse(&params.schema, &Limits::new()).unwrap(),
            &params.recipient,
            NOW,
        )
        .unwrap();
    assert_eq!(collector.holder(), prover.id());
    let mut sealed = BTreeMap::new();
    sealed.insert(
        "password".to_string(),
        collector.seal("password", "correct horse battery").unwrap(),
    );
    sealed.insert(
        "note".to_string(),
        collector.seal("note", "for ana").unwrap(),
    );
    let mut plain = Map::new();
    plain.insert("user".into(), json!("ana"));
    plain.insert("port".into(), json!(5432));
    let reply = RequestReply::Accept { plain, sealed }.to_value();
    let wire = reply.to_string();
    assert!(
        !wire.contains("correct horse"),
        "the answer holds no secret in the clear"
    );
    assert!(!wire.contains("for ana"));

    let RequestReply::Accept { plain, sealed } =
        RequestReply::from_value(&serde_json::from_str(&wire).unwrap(), |name| {
            form.field(name).is_some_and(|field| field.secret)
        })
        .unwrap()
    else {
        panic!("accepted")
    };
    let opened = receiver.open_values(&plain, &sealed).unwrap();
    assert_eq!(opened.plain["user"], "ana");
    assert_eq!(
        opened.secrets["password"].expose_str(),
        "correct horse battery"
    );
    assert_eq!(opened.secrets["note"].expose(), b"for ana");
    let shown = format!("{:?} {:?} {:?} {:?}", opened, receiver, prover, collector);
    assert!(
        !shown.contains("correct horse") && !shown.contains("for ana"),
        "no Debug prints a value: {shown}"
    );
}

#[test]
fn a_show_asks_the_frontend_for_a_key_then_delivers() {
    let collector_secret = x25519(7);
    let frontend = Prover::x25519(&x25519(9), &x25519_spki(&collector_secret)).unwrap();
    let schema = json!({"type": "object", "properties": {"label": {"type": "string"}, "key": {"type": "string", "writeOnly": true}}});
    let form = Form::parse(&schema, &Limits::new()).unwrap();
    let mut values = Map::new();
    values.insert("label".into(), json!("ci"));
    let open = ShowOpen {
        session_id: "s1".into(),
        request_id: "show-1".into(),
        message: "Your new key".into(),
        schema: schema.clone(),
        form: None,
        values: values.clone(),
    };
    let open = ShowOpen::from_value(&open.to_value()).unwrap();
    form.check_plain(&open.values).unwrap();

    // The frontend makes the key pair for this show and answers the open step.
    let receiver = Receiver::new(
        Method::Show,
        &*open.request_id,
        &form,
        &frontend,
        NOW,
        Duration::from_secs(300),
    )
    .unwrap();
    let ready = ShowOpened::from_value(&ShowOpened::Ready(receiver.recipient().clone()).to_value())
        .unwrap();
    let ShowOpened::Ready(recipient) = ready else {
        panic!("ready")
    };

    // The host verifies it against the frontend it paired with, and seals.
    let host = Verifier::builder()
        .trust(frontend.id())
        .x25519_identity(&collector_secret)
        .unwrap()
        .build();
    let collector = host
        .collector(Method::Show, &open.request_id, &form, &recipient, NOW)
        .unwrap();
    let mut sealed = BTreeMap::new();
    sealed.insert(
        "key".to_string(),
        collector.seal("key", "sk_live_123").unwrap(),
    );
    let deliver = ShowDeliver {
        session_id: "s1".into(),
        request_id: "show-1".into(),
        sealed,
    };
    let deliver =
        ShowDeliver::from_value(&serde_json::from_str(&deliver.to_value().to_string()).unwrap())
            .unwrap();

    let opened = receiver.open_values(&open.values, &deliver.sealed).unwrap();
    assert_eq!(opened.secrets["key"].expose_str(), "sk_live_123");
    assert_eq!(
        ShowDone::from_value(&ShowDone::Shown.to_value()).unwrap(),
        ShowDone::Shown
    );
}

#[test]
fn a_ciphertext_moved_to_another_field_or_request_is_refused() {
    let prover = Prover::ecdsa_p256(&scalar()).unwrap();
    let form = form();
    let one = Receiver::new(
        Method::Request,
        "r1",
        &form,
        &prover,
        NOW,
        Duration::from_secs(300),
    )
    .unwrap();
    let verifier = Verifier::builder().trust(prover.id()).build();
    let collector = verifier
        .collector(Method::Request, "r1", &form, one.recipient(), NOW)
        .unwrap();
    let sealed = collector.seal("password", "correct horse battery").unwrap();
    assert_eq!(
        one.open("note", &sealed).unwrap_err(),
        Error::Open,
        "another field"
    );
    assert!(one.open("password", &sealed).is_ok());

    // The same key pair would not matter: the request id is bound too. A receiver for r2 that held r1's key cannot be made
    // here, so a sealing for r2 to r1's key is what a moved ciphertext looks like to r1.
    let moved = verifier.collector(Method::Request, "r2", &form, one.recipient(), NOW);
    assert_eq!(
        moved.unwrap_err(),
        Error::BadProof,
        "the binding names the request it was made for"
    );
    assert_eq!(
        one.open("user", &sealed).unwrap_err(),
        Error::Field("not a secret question of the form")
    );
}

#[test]
fn a_key_nobody_knows_or_whose_binding_fails_is_refused() {
    let prover = Prover::ecdsa_p256(&scalar()).unwrap();
    let stranger = Prover::ecdsa_p256(&(2u8..=33).collect::<Vec<_>>()).unwrap();
    let form = form();
    let receiver = Receiver::new(
        Method::Request,
        "r1",
        &form,
        &stranger,
        NOW,
        Duration::from_secs(300),
    )
    .unwrap();
    let recipient = receiver.recipient().clone();

    let knows_only_prover = Verifier::builder().trust(prover.id()).build();
    assert_eq!(
        knows_only_prover
            .collector(Method::Request, "r1", &form, &recipient, NOW)
            .unwrap_err(),
        Error::UnknownIdentity
    );

    // A pipe that swaps in its own key under the stranger's binding is a bad proof, not an accepted key.
    let knows_stranger = Verifier::builder().trust(stranger.id()).build();
    let mut swapped = recipient.clone();
    swapped.key[0] ^= 0xff;
    assert_eq!(
        knows_stranger
            .collector(Method::Request, "r1", &form, &swapped, NOW)
            .unwrap_err(),
        Error::BadProof
    );

    // A binding that names the prover's key but was made by the stranger.
    let mut forged = recipient;
    forged.binding.key = prover.spki().to_vec();
    assert_eq!(
        knows_only_prover
            .collector(Method::Request, "r1", &form, &forged, NOW)
            .unwrap_err(),
        Error::BadProof
    );

    assert_eq!(
        knows_stranger
            .collector(
                Method::Request,
                "r1",
                &form,
                receiver.recipient(),
                NOW + 301
            )
            .unwrap_err(),
        Error::Expired
    );
    assert!(
        knows_stranger
            .collector(Method::Request, "r1", &form, receiver.recipient(), NOW)
            .is_ok()
    );
    assert_eq!(receiver.recipient().binding.method, BINDING_ECDSA);
}

#[test]
fn a_diffie_hellman_binding_needs_the_collectors_own_identity_and_audience() {
    let collector_secret = x25519(7);
    let holder = Prover::x25519(&x25519(9), &x25519_spki(&collector_secret)).unwrap();
    let form = form();
    let receiver = Receiver::new(
        Method::Request,
        "r1",
        &form,
        &holder,
        NOW,
        Duration::from_secs(300),
    )
    .unwrap();
    let recipient = receiver.recipient();
    assert!(recipient.binding.audience.is_some());

    let without_own = Verifier::builder().trust(holder.id()).build();
    assert!(matches!(
        without_own.collector(Method::Request, "r1", &form, recipient, NOW),
        Err(Error::Unsupported(_))
    ));
    let other = Verifier::builder()
        .trust(holder.id())
        .x25519_identity(&x25519(8))
        .unwrap()
        .build();
    assert_eq!(
        other
            .collector(Method::Request, "r1", &form, recipient, NOW)
            .unwrap_err(),
        Error::WrongAudience
    );
    let right = Verifier::builder()
        .trust(holder.id())
        .x25519_identity(&collector_secret)
        .unwrap()
        .build();
    assert!(
        right
            .collector(Method::Request, "r1", &form, recipient, NOW)
            .is_ok()
    );
    // A binding with the audience taken off is not the one that was proved.
    let mut bare = recipient.clone();
    bare.binding.audience = None;
    assert!(
        right
            .collector(Method::Request, "r1", &form, &bare, NOW)
            .is_err()
    );
}

#[test]
fn a_collector_seals_only_secret_fields_that_fit() {
    let prover = Prover::ecdsa_p256(&scalar()).unwrap();
    let form = form();
    let receiver = Receiver::new(
        Method::Request,
        "r1",
        &form,
        &prover,
        NOW,
        Duration::from_secs(300),
    )
    .unwrap();
    let collector = Verifier::builder()
        .trust(prover.id())
        .build()
        .collector(Method::Request, "r1", &form, receiver.recipient(), NOW)
        .unwrap();
    assert!(
        matches!(collector.seal("user", "ana"), Err(Error::Field(_))),
        "a plain field is not sealed"
    );
    assert!(
        matches!(collector.seal("password", "short"), Err(Error::Field(_))),
        "a value under its minimum length"
    );
    assert!(matches!(
        collector.seal("nothing", "x"),
        Err(Error::Field(_))
    ));
    assert!(collector.is_secret("password") && !collector.is_secret("user"));
    // A required secret left out, a sealed value for a plain question and a plain value for a secret one are all refused.
    let sealed = BTreeMap::from([(
        "password".to_string(),
        collector.seal("password", "correct horse").unwrap(),
    )]);
    let plain = |value: Value| value.as_object().unwrap().clone();
    assert!(
        receiver
            .open_values(&plain(json!({"user": "ana"})), &sealed)
            .is_ok()
    );
    assert!(
        receiver
            .open_values(&plain(json!({"user": "ana"})), &BTreeMap::new())
            .is_err(),
        "required password missing"
    );
    assert!(
        receiver
            .open_values(&plain(json!({"user": "ana", "password": "x"})), &sealed)
            .is_err()
    );
    assert!(
        receiver.open_values(&plain(json!({})), &sealed).is_err(),
        "required user missing"
    );
    let wrong = BTreeMap::from([("user".to_string(), sealed["password"].clone())]);
    assert!(
        receiver
            .open_values(&plain(json!({"user": "ana"})), &wrong)
            .is_err()
    );
}

#[test]
fn the_capability_is_read_only_at_its_version() {
    let all = Capability::all();
    let meta = json!({CAPABILITY: all.to_value()});
    assert_eq!(Capability::from_meta(meta.as_object().unwrap()), Some(all));
    for other in [
        json!({}),
        json!({CAPABILITY: {"version": 2, "methods": ["request"]}}),
        json!({CAPABILITY: {"version": 1, "methods": []}}),
        json!({CAPABILITY: true}),
    ] {
        assert_eq!(
            Capability::from_meta(other.as_object().unwrap()),
            None,
            "{other}"
        );
    }
}

#[test]
fn answers_decline_and_cancel_read_back() {
    for reply in [RequestReply::Decline, RequestReply::Cancel] {
        assert_eq!(
            RequestReply::from_value(&reply.to_value(), |_| false).unwrap(),
            reply
        );
    }
    assert!(RequestReply::from_value(&json!({"action": "maybe"}), |_| false).is_err());
    assert!(
        RequestReply::from_value(
            &json!({"action": "accept", "values": {"pw": "plain text"}}),
            |name| name == "pw"
        )
        .is_err()
    );
}

proptest! {
    #[test]
    fn arbitrary_text_never_panics_a_reader(text in ".{0,200}", other in ".{0,200}") {
        let _ = text.parse::<IdentityId>();
        let _ = Sealed::from_value(&json!({"enc": text, "ct": other}));
        let _ = Sealed::from_value(&json!(text));
        let _ = coqui_acp::secret::Binding::from_value(&json!({"method": text, "key": other, "expires": 1, "proof": ""}));
        let _ = Form::parse(&json!({"properties": {text.clone(): {"type": other, "writeOnly": true}}}), &Limits::new());
    }
}
