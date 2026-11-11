//! The secret-prompt extension through its public API: a request and a show end to end, and what each refuses.

#![cfg(feature = "secret")]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use coqui_acp::secret::unprotected;
use coqui_acp::secret::{
    BINDING_ECDSA, CAPABILITY, Capability, Context, DEFAULT_LIFETIME, Error, Form, IdentityId,
    Limits, Method, Prover, Receiver, RequestParams, RequestReply, ShowDeliver, ShowDone, ShowOpen,
    ShowOpened, Verifier,
};
use p256::ecdsa::signature::Signer as _;
use proptest::prelude::*;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

const NOW: u64 = 1_700_000_000;

fn schema_text() -> String {
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
    .to_string()
}

fn context(request_id: &str) -> Context {
    Context {
        session_id: "s1".into(),
        request_id: request_id.into(),
        message: "The deploy needs a login".into(),
        schema: schema_text(),
        form: None,
    }
}

fn scalar(first: u8) -> Vec<u8> {
    (first..first + 32).collect()
}

/// Two ECDSA identities that know each other: the host and the frontend.
struct Pair {
    host: Prover,
    frontend: Prover,
}

fn pair() -> Pair {
    Pair {
        host: unprotected::ecdsa_p256(&scalar(1)).unwrap(),
        frontend: unprotected::ecdsa_p256(&scalar(2)).unwrap(),
    }
}

fn verifier_for(me: &Prover, trusts: &Prover) -> Verifier {
    Verifier::builder(me.id()).trust(trusts.id()).build()
}

fn form() -> Form {
    Form::parse_text(&schema_text(), &Limits::new()).unwrap()
}

/// The frontend answers a request: it verifies the host's key, seals the secrets, authenticates them and writes the reply.
fn answer(
    pair: &Pair,
    params: &RequestParams,
    values: &[(&str, &str)],
    plain: Map<String, Value>,
) -> Value {
    let frontend_side = verifier_for(&pair.frontend, &pair.host);
    let collector = frontend_side
        .collector(
            &pair.host.id(),
            Method::Request,
            &params.context,
            &params.recipient,
            NOW,
        )
        .unwrap();
    let sealed: BTreeMap<_, _> = values
        .iter()
        .map(|(name, value)| ((*name).to_string(), collector.seal(name, value).unwrap()))
        .collect();
    let auth = collector
        .authenticate(&pair.frontend, &sealed, NOW, DEFAULT_LIFETIME)
        .unwrap();
    RequestReply::Accept {
        plain,
        sealed,
        auth,
    }
    .to_value()
}

fn request(pair: &Pair, id: &str) -> (Receiver, RequestParams) {
    let receiver = Receiver::new(
        Method::Request,
        &context(id),
        &Limits::new(),
        &pair.host,
        &pair.frontend.id(),
        NOW,
        DEFAULT_LIFETIME,
    )
    .unwrap();
    let params = RequestParams {
        context: context(id),
        recipient: receiver.recipient().clone(),
    };
    // The host's message crosses a pipe: only JSON survives.
    let params =
        RequestParams::from_value(&serde_json::from_str(&params.to_value().to_string()).unwrap())
            .unwrap();
    (receiver, params)
}

fn plain_user() -> Map<String, Value> {
    let mut plain = Map::new();
    plain.insert("user".into(), json!("ana"));
    plain.insert("port".into(), json!(5432));
    plain
}

#[test]
fn a_request_reaches_the_host_sealed_and_nothing_else_holds_the_value() {
    let pair = pair();
    let form = form();
    let (receiver, params) = request(&pair, "r1");
    let reply = answer(
        &pair,
        &params,
        &[("password", "correct horse battery"), ("note", "for ana")],
        plain_user(),
    );
    let wire = reply.to_string();
    assert!(
        !wire.contains("correct horse") && !wire.contains("for ana"),
        "the answer holds no secret in the clear"
    );
    let RequestReply::Accept {
        plain,
        sealed,
        auth,
    } = RequestReply::from_value(&serde_json::from_str(&wire).unwrap(), |name| {
        form.field(name).is_some_and(|field| field.secret)
    })
    .unwrap()
    else {
        panic!("accepted")
    };
    let shown = format!(
        "{receiver:?} {:?} {:?}",
        pair.host,
        verifier_for(&pair.host, &pair.frontend)
    );
    let opened = receiver
        .open_values(
            &verifier_for(&pair.host, &pair.frontend),
            &pair.frontend.id(),
            &plain,
            &sealed,
            &auth,
            NOW,
        )
        .unwrap();
    assert_eq!(opened.plain["user"], "ana");
    assert_eq!(
        opened.secrets["password"].expose_str(),
        "correct horse battery"
    );
    assert_eq!(opened.secrets["note"].expose(), b"for ana");
    let shown = format!("{shown} {opened:?}");
    assert!(
        !shown.contains("correct horse") && !shown.contains("for ana"),
        "no Debug prints a value: {shown}"
    );
}

#[test]
fn a_show_asks_the_frontend_for_a_key_then_delivers_authenticated_values() {
    // Noise identities: neither can sign, each is paired with the other.
    let (host_spki, host_agree) = unprotected::x25519_agreement(&[7; 32]).unwrap();
    let (frontend_spki, frontend_agree) = unprotected::x25519_agreement(&[9; 32]).unwrap();
    let host = unprotected::x25519(&[7; 32], &frontend_spki).unwrap();
    let frontend = unprotected::x25519(&[9; 32], &host_spki).unwrap();
    let (host_id, frontend_id) = (host.id(), frontend.id());
    let host_verifier = Verifier::builder(host_id)
        .trust(frontend_id)
        .agreement(&host_spki, host_agree)
        .unwrap()
        .build();
    let frontend_verifier = Verifier::builder(frontend_id)
        .trust(host_id)
        .agreement(&frontend_spki, frontend_agree)
        .unwrap()
        .build();

    let schema = json!({"type": "object", "properties": {"label": {"type": "string"}, "key": {"type": "string", "writeOnly": true}}}).to_string();
    let mut values = Map::new();
    values.insert("label".into(), json!("ci"));
    let open = ShowOpen {
        context: Context {
            session_id: "s1".into(),
            request_id: "show-1".into(),
            message: "Your new key".into(),
            schema,
            form: None,
        },
        values,
    };
    let open = ShowOpen::from_value(&open.to_value()).unwrap();

    // The frontend makes the key pair for this show and answers the open step.
    let receiver = Receiver::new(
        Method::Show,
        &open.context,
        &Limits::new(),
        &frontend,
        &host_id,
        NOW,
        DEFAULT_LIFETIME,
    )
    .unwrap();
    let ShowOpened::Ready(recipient) =
        ShowOpened::from_value(&ShowOpened::Ready(receiver.recipient().clone()).to_value())
            .unwrap()
    else {
        panic!("ready")
    };

    // The host verifies it against the frontend it paired with, seals, and authenticates the delivery.
    let collector = host_verifier
        .collector(&frontend_id, Method::Show, &open.context, &recipient, NOW)
        .unwrap();
    let mut sealed = BTreeMap::new();
    sealed.insert(
        "key".to_string(),
        collector.seal("key", "sk_live_123").unwrap(),
    );
    let auth = collector
        .authenticate(&host, &sealed, NOW, DEFAULT_LIFETIME)
        .unwrap();
    let deliver = ShowDeliver {
        session_id: "s1".into(),
        request_id: "show-1".into(),
        sealed,
        auth,
    };
    let deliver =
        ShowDeliver::from_value(&serde_json::from_str(&deliver.to_value().to_string()).unwrap())
            .unwrap();
    let opened = receiver
        .open_values(
            &frontend_verifier,
            &host_id,
            &open.values,
            &deliver.sealed,
            &deliver.auth,
            NOW,
        )
        .unwrap();
    assert_eq!(opened.secrets["key"].expose_str(), "sk_live_123");
    assert_eq!(
        ShowDone::from_value(&ShowDone::Shown.to_value()).unwrap(),
        ShowDone::Shown
    );
}

#[test]
fn a_form_changed_on_the_way_is_refused_before_it_is_drawn() {
    let pair = pair();
    let (_, params) = request(&pair, "r1");
    let frontend_side = verifier_for(&pair.frontend, &pair.host);
    let take = |context: &Context| {
        frontend_side
            .collector(
                &pair.host.id(),
                Method::Request,
                context,
                &params.recipient,
                NOW,
            )
            .map(|_| ())
    };
    assert_eq!(take(&params.context), Ok(()));
    let mut downgraded = params.context.clone();
    downgraded.schema = downgraded
        .schema
        .replace("\"writeOnly\":true", "\"writeOnly\":false");
    assert_eq!(
        take(&downgraded),
        Err(Error::BadProof),
        "a secret made plain"
    );
    let mut reworded = params.context.clone();
    reworded.message = "Send the money".into();
    assert_eq!(take(&reworded), Err(Error::BadProof));
    let mut elsewhere = params.context.clone();
    elsewhere.session_id = "s2".into();
    assert_eq!(take(&elsewhere), Err(Error::BadProof));
    let mut relaid = params.context.clone();
    relaid.form = Some("{}".into());
    assert_eq!(take(&relaid), Err(Error::BadProof));
}

#[test]
fn only_the_expected_identity_is_taken_though_others_are_known() {
    let pair = pair();
    let other = unprotected::ecdsa_p256(&scalar(3)).unwrap();
    let (_, params) = request(&pair, "r1");
    let knows_both = Verifier::builder(pair.frontend.id())
        .trust(pair.host.id())
        .trust(other.id())
        .build();
    let take = |expected: IdentityId| {
        knows_both
            .collector(
                &expected,
                Method::Request,
                &params.context,
                &params.recipient,
                NOW,
            )
            .map(|_| ())
    };
    assert_eq!(take(pair.host.id()), Ok(()));
    assert_eq!(take(other.id()), Err(Error::UnknownIdentity));
}

/// A whole request answered: the receiver, and what the frontend sent back.
fn flow(
    pair: &Pair,
) -> (
    Receiver,
    Map<String, Value>,
    BTreeMap<String, coqui_acp::secret::Sealed>,
    coqui_acp::secret::Binding,
) {
    let (receiver, params) = request(pair, "r1");
    let reply = answer(
        pair,
        &params,
        &[("password", "correct horse battery")],
        plain_user(),
    );
    let form = form();
    let RequestReply::Accept {
        plain,
        sealed,
        auth,
    } = RequestReply::from_value(&reply, |name| form.field(name).is_some_and(|f| f.secret))
        .unwrap()
    else {
        panic!("accepted")
    };
    (receiver, plain, sealed, auth)
}

#[test]
fn values_are_opened_only_from_the_sender_that_authenticated_them() {
    let pair = pair();
    let stranger = unprotected::ecdsa_p256(&scalar(3)).unwrap();
    let host_side = verifier_for(&pair.host, &pair.frontend);

    // A ciphertext changed after it was authenticated.
    let (receiver, plain, mut sealed, auth) = flow(&pair);
    sealed.get_mut("password").unwrap().ct[0] ^= 1;
    assert_eq!(
        receiver
            .open_values(&host_side, &pair.frontend.id(), &plain, &sealed, &auth, NOW)
            .unwrap_err(),
        Error::BadProof
    );
    // A sender other than the one expected.
    let (receiver, plain, sealed, auth) = flow(&pair);
    assert_eq!(
        receiver
            .open_values(&host_side, &stranger.id(), &plain, &sealed, &auth, NOW)
            .unwrap_err(),
        Error::UnknownIdentity
    );
    let (receiver, plain, sealed, auth) = flow(&pair);
    assert!(
        receiver
            .open_values(&host_side, &pair.frontend.id(), &plain, &sealed, &auth, NOW)
            .is_ok()
    );
}

#[test]
fn a_signer_that_signs_with_a_high_s_still_binds_and_a_high_s_binding_is_refused() {
    // A browser's signer does not normalize s: half of its signatures are high.
    let key = p256::ecdsa::SigningKey::from_slice(&scalar(1)).unwrap();
    let mut spki = vec![
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    spki.extend_from_slice(key.verifying_key().to_sec1_point(false).as_bytes());
    let high = Prover::signing(spki, move |message| {
        let signature: p256::ecdsa::Signature = key.sign(message);
        let (r, s) = signature.split_scalars();
        let flipped = p256::ecdsa::Signature::from_scalars(r, -*s).unwrap();
        Ok(flipped.to_bytes().into())
    })
    .unwrap();
    let frontend = unprotected::ecdsa_p256(&scalar(2)).unwrap();
    let receiver = Receiver::new(
        Method::Request,
        &context("r1"),
        &Limits::new(),
        &high,
        &frontend.id(),
        NOW,
        DEFAULT_LIFETIME,
    )
    .unwrap();
    let verifier = Verifier::builder(frontend.id()).trust(high.id()).build();
    assert!(
        verifier
            .collector(
                &high.id(),
                Method::Request,
                &context("r1"),
                receiver.recipient(),
                NOW
            )
            .is_ok()
    );
    assert_eq!(receiver.recipient().binding.method, BINDING_ECDSA);
}

#[test]
fn a_binding_is_taken_to_the_skew_and_no_further() {
    let pair = pair();
    let (_, params) = request(&pair, "r1");
    let at = |now: u64, skew: u64| {
        Verifier::builder(pair.frontend.id())
            .trust(pair.host.id())
            .skew(Duration::from_secs(skew))
            .build()
            .collector(
                &pair.host.id(),
                Method::Request,
                &params.context,
                &params.recipient,
                now,
            )
            .map(|_| ())
    };
    let end = NOW + DEFAULT_LIFETIME.as_secs();
    assert_eq!(at(end, 0), Ok(()));
    assert_eq!(at(end + 1, 0), Err(Error::Expired));
    assert_eq!(at(end + 30, 30), Ok(()));
    assert_eq!(at(end + 31, 30), Err(Error::Expired));
    // A receiver clock ahead of the collector's by more than the longest binding plus the skew.
    assert_eq!(at(NOW - 90, 30), Ok(()));
    assert_eq!(at(NOW - 91, 30), Err(Error::Expired));
    assert!(
        DEFAULT_LIFETIME < Duration::from_secs(300),
        "the default stays under the collector's default limit"
    );
}

#[test]
fn a_diffie_hellman_binding_needs_the_collectors_own_key_agreement() {
    let (host_spki, _) = unprotected::x25519_agreement(&[7; 32]).unwrap();
    let (frontend_spki, frontend_agree) = unprotected::x25519_agreement(&[9; 32]).unwrap();
    let host = unprotected::x25519(&[7; 32], &frontend_spki).unwrap();
    let frontend_id = IdentityId::of(&frontend_spki);
    let receiver = Receiver::new(
        Method::Request,
        &context("r1"),
        &Limits::new(),
        &host,
        &frontend_id,
        NOW,
        DEFAULT_LIFETIME,
    )
    .unwrap();
    // A host paired with the frontend cannot bind for anyone else.
    let elsewhere = IdentityId::of(&host_spki);
    assert!(matches!(
        Receiver::new(
            Method::Request,
            &context("r1"),
            &Limits::new(),
            &host,
            &elsewhere,
            NOW,
            DEFAULT_LIFETIME
        ),
        Err(Error::WrongAudience)
    ));
    let without = Verifier::builder(frontend_id).trust(host.id()).build();
    assert!(matches!(
        without.collector(
            &host.id(),
            Method::Request,
            &context("r1"),
            receiver.recipient(),
            NOW
        ),
        Err(Error::Unsupported(_))
    ));
    let with = Verifier::builder(frontend_id)
        .trust(host.id())
        .agreement(&frontend_spki, frontend_agree)
        .unwrap()
        .build();
    assert!(
        with.collector(
            &host.id(),
            Method::Request,
            &context("r1"),
            receiver.recipient(),
            NOW
        )
        .is_ok()
    );
    // The agreement has to be the verifier's own identity.
    let (other_spki, other_agree) = unprotected::x25519_agreement(&[5; 32]).unwrap();
    assert!(
        Verifier::builder(frontend_id)
            .agreement(&other_spki, other_agree)
            .is_err()
    );
}

#[test]
fn a_collector_seals_only_secret_fields_that_fit() {
    let pair = pair();
    let (_, params) = request(&pair, "r1");
    let collector = verifier_for(&pair.frontend, &pair.host)
        .collector(
            &pair.host.id(),
            Method::Request,
            &params.context,
            &params.recipient,
            NOW,
        )
        .unwrap();
    assert!(matches!(
        collector.seal("user", "ana"),
        Err(Error::Field(_))
    ));
    assert!(matches!(
        collector.seal("password", "short"),
        Err(Error::Field(_))
    ));
    assert!(matches!(
        collector.seal("nothing", "x"),
        Err(Error::Field(_))
    ));
    assert!(collector.is_secret("password") && !collector.is_secret("user"));
    assert_eq!(collector.form().fields.len(), 4);
}

#[test]
fn a_required_secret_left_out_or_a_plain_value_for_a_secret_is_refused() {
    let pair = pair();
    let host_side = verifier_for(&pair.host, &pair.frontend);
    for (values, plain) in [
        (vec![], plain_user()),
        (vec![("password", "correct horse")], {
            let mut plain = plain_user();
            plain.insert("password".into(), json!("x"));
            plain
        }),
        (vec![("password", "correct horse")], Map::new()),
    ] {
        let (receiver, params) = request(&pair, "r1");
        let frontend_side = verifier_for(&pair.frontend, &pair.host);
        let collector = frontend_side
            .collector(
                &pair.host.id(),
                Method::Request,
                &params.context,
                &params.recipient,
                NOW,
            )
            .unwrap();
        let sealed: BTreeMap<_, _> = values
            .iter()
            .map(|(name, value)| ((*name).to_string(), collector.seal(name, value).unwrap()))
            .collect();
        let auth = collector
            .authenticate(&pair.frontend, &sealed, NOW, DEFAULT_LIFETIME)
            .unwrap();
        assert!(matches!(
            receiver.open_values(&host_side, &pair.frontend.id(), &plain, &sealed, &auth, NOW),
            Err(Error::Field(_))
        ));
    }
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
        let _ = coqui_acp::secret::Sealed::from_value(&json!({"enc": text, "ct": other}));
        let _ = coqui_acp::secret::Binding::from_value(&json!({"method": text, "key": other, "expires": 1, "audience": text, "proof": ""}));
        let _ = Form::parse_text(&text, &Limits::new());
        let _ = Form::parse_text(&format!("{{\"properties\":{{{text:?}:{{\"type\":{other:?},\"writeOnly\":true}}}}}}"), &Limits::new());
        let _ = RequestParams::from_value(&json!({"sessionId": text, "schema": other}));
    }
}
