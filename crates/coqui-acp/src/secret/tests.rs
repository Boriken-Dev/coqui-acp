//! `vectors/secret.json`: RFC 9180's base-mode vector run through this crate's suite, and the extension's own bytes (key bindings,
//! sealed fields, refusals) that the Dart client must reproduce or refuse alike.
//!
//! The RFC section is copied from the RFC. The rest is written by `vectors_are_current` when `COQUI_ACP_WRITE_VECTORS` is set, from
//! the fixed inputs below, and is read back and checked independently of how it was made.

use super::binding::{Method, Prover, Verifier, statement};
use super::form::{Form, Limits};
use super::seal::testing::{field_info, key_pair, open_raw, seal_raw, seal_with};
use super::seal::{Receiver, Recipient, Sealed};
use super::{Error, IdentityId};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;
use data_encoding::HEXLOWER;
use hpke::Serializable;
use serde_json::{Map, Value, json};

const VECTORS: &str = include_str!("../../../../vectors/secret.json");

fn hex(text: &str) -> Vec<u8> {
    HEXLOWER.decode(text.as_bytes()).expect("hex")
}

fn kind(error: &Error) -> &'static str {
    match error {
        Error::Schema(_) => "schema",
        Error::Malformed(_) => "malformed",
        Error::Unsupported(_) => "unsupported",
        Error::Key(_) => "key",
        Error::UnknownIdentity => "unknown_identity",
        Error::BadProof => "bad_proof",
        Error::Expired => "expired",
        Error::WrongAudience => "wrong_audience",
        Error::Field(_) => "field",
        Error::Seal => "seal",
        Error::Open => "open",
        Error::TooLarge(_) => "too_large",
    }
}

fn schema() -> Value {
    json!({
        "type": "object",
        "title": "Registry login",
        "properties": {
            "user": {"type": "string", "title": "User", "minLength": 1},
            "password": {"type": "string", "title": "Password", "writeOnly": true, "minLength": 4},
            "token": {"type": "string", "x-sensitive": true, "contentEncoding": "base64"},
            "pin": {"type": "integer", "writeOnly": true, "minimum": 0, "maximum": 999999}
        },
        "required": ["user", "password"]
    })
}

/// A scalar and keys the cases below use; none is secret, all are published test values (RFC 6979 and RFC 7748).
const ECDSA_SCALAR: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
const NOISE_HOLDER: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
const NOISE_COLLECTOR: &str = "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb";
const NOW: u64 = 1_700_000_000;
const LIFETIME: u64 = 300;

fn ikm(start: u8) -> Vec<u8> {
    (start..start + 32).collect()
}

fn x25519_spki(public: &[u8]) -> Vec<u8> {
    let mut spki = hex("302a300506032b656e032100");
    spki.extend_from_slice(public);
    spki
}

fn x25519_public(secret: &[u8]) -> Vec<u8> {
    let secret: [u8; 32] = secret.try_into().unwrap();
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(secret))
        .as_bytes()
        .to_vec()
}

struct Case {
    name: &'static str,
    prover: Prover,
    method: Method,
    request_id: &'static str,
}

fn cases() -> Vec<Case> {
    let ecdsa = Prover::ecdsa_p256(&hex(ECDSA_SCALAR)).unwrap();
    let collector_spki = x25519_spki(&x25519_public(&hex(NOISE_COLLECTOR)));
    let noise = Prover::x25519(&hex(NOISE_HOLDER), &collector_spki).unwrap();
    vec![
        Case {
            name: "a webcrypto identity signs a request's key",
            prover: ecdsa,
            method: Method::Request,
            request_id: "6f1c8b6e-2d0e-4a52-9a43-0a6d3a8b8d11",
        },
        Case {
            name: "a noise identity proves a show's key by Diffie-Hellman",
            prover: noise,
            method: Method::Show,
            request_id: "show-1",
        },
    ]
}

fn form() -> Form {
    Form::parse(&schema(), &Limits::new()).unwrap()
}

fn sealed_values() -> Vec<(&'static str, &'static str)> {
    vec![
        ("password", "correct horse"),
        ("token", "c2VjcmV0LXRva2Vu"),
        ("pin", "4821"),
    ]
}

/// The vectors this crate writes: everything but the RFC's section.
fn generate() -> Map<String, Value> {
    let form = form();
    let mut bindings = Vec::new();
    let mut sealed = Vec::new();
    for case in cases() {
        let (private, public) = key_pair(&ikm(0));
        let receiver = Receiver::from_key_pair(
            case.method,
            case.request_id.into(),
            &form,
            &case.prover,
            NOW,
            Duration::from_secs(LIFETIME),
            private,
            &public,
        )
        .unwrap();
        let recipient = receiver.recipient().clone();
        let expires = recipient.binding.expires;
        let signed = statement(
            case.method,
            case.request_id,
            &recipient.key,
            expires,
            recipient.binding.audience.as_ref(),
        );
        bindings.push(json!({
            "name": case.name,
            "kind": if recipient.binding.method == super::BINDING_ECDSA { "ecdsa" } else { "dh" },
            "private": if recipient.binding.method == super::BINDING_ECDSA { ECDSA_SCALAR } else { NOISE_HOLDER },
            "collector_private": if recipient.binding.method == super::BINDING_ECDSA { Value::Null } else { Value::from(NOISE_COLLECTOR) },
            "method": case.method.as_str(),
            "request_id": case.request_id,
            "recipient_ikm": HEXLOWER.encode(&ikm(0)),
            "now": NOW,
            "lifetime": LIFETIME,
            "identity": case.prover.id().to_string(),
            "statement": HEXLOWER.encode(&signed),
            "recipient": recipient.to_value(),
        }));
        let mut fields = Vec::new();
        for (name, value) in sealed_values() {
            let made = seal_with(
                &recipient.key,
                case.method,
                case.request_id,
                name,
                value.as_bytes(),
                &ikm(32),
            );
            fields.push(json!({"field": name, "value": value, "ephemeral_ikm": HEXLOWER.encode(&ikm(32)), "sealed": made.to_value()}));
        }
        sealed.push(
            json!({"name": case.name, "binding": case.name, "schema": schema(), "fields": fields}),
        );
    }
    let mut out = Map::new();
    out.insert("info".into(), json!([
        {"method": "request", "request_id": "r1", "field": "password", "info": HEXLOWER.encode(&field_info(Method::Request, "r1", "password"))},
        {"method": "show", "request_id": "r1", "field": "password", "info": HEXLOWER.encode(&field_info(Method::Show, "r1", "password"))},
        {"method": "request", "request_id": "r1", "field": "user", "info": HEXLOWER.encode(&field_info(Method::Request, "r1", "user"))},
    ]));
    out.insert("bindings".into(), Value::Array(bindings));
    out.insert("sealed".into(), Value::Array(sealed));
    out.insert("forms".into(), forms());
    out.insert("refusals".into(), refusals());
    out
}

fn forms() -> Value {
    let flat = |property: Value| json!({"type": "object", "properties": {"f": property}});
    json!({
        "accepted": [
            {"name": "plain and secret fields", "schema": schema(), "secret": ["password", "token", "pin"], "plain": ["user"]},
            {"name": "a secret marked x-sensitive", "schema": flat(json!({"type": "string", "x-sensitive": true})), "secret": ["f"], "plain": []},
            {"name": "a plain choice", "schema": flat(json!({"type": "string", "oneOf": [{"const": "a", "title": "A"}]})), "secret": [], "plain": ["f"]}
        ],
        "refused": [
            {"name": "not an object", "schema": [], "kind": "schema"},
            {"name": "no properties", "schema": {"type": "object"}, "kind": "schema"},
            {"name": "no questions", "schema": {"type": "object", "properties": {}}, "kind": "schema"},
            {"name": "a type that is not object", "schema": {"type": "array", "properties": {"f": {"type": "string"}}}, "kind": "schema"},
            {"name": "a property that is not an object", "schema": {"properties": {"f": 3}}, "kind": "schema"},
            {"name": "a type no control shows", "schema": flat(json!({"type": "object"})), "kind": "schema"},
            {"name": "an empty name", "schema": {"properties": {"": {"type": "string"}}}, "kind": "schema"},
            {"name": "required names a missing property", "schema": {"properties": {"f": {"type": "string"}}, "required": ["g"]}, "kind": "schema"},
            {"name": "required is not a list", "schema": {"properties": {"f": {"type": "string"}}, "required": "f"}, "kind": "schema"},
            {"name": "a secret choice", "schema": flat(json!({"type": "string", "writeOnly": true, "enum": ["a", "b"]})), "kind": "schema"},
            {"name": "a secret flag", "schema": flat(json!({"type": "boolean", "writeOnly": true})), "kind": "schema"},
            {"name": "a secret with a default", "schema": flat(json!({"type": "string", "writeOnly": true, "default": "hunter2"})), "kind": "schema"},
            {"name": "a secret with a const", "schema": flat(json!({"type": "string", "x-sensitive": true, "const": "x"})), "kind": "schema"},
            {"name": "a secret with examples", "schema": flat(json!({"type": "string", "writeOnly": true, "examples": ["x"]})), "kind": "schema"}
        ]
    })
}

fn refusals() -> Value {
    json!([
        {"case": "verify", "name": "an identity nobody knows", "base": "a webcrypto identity signs a request's key", "change": {"trusted": false}, "expect": "unknown_identity"},
        {"case": "verify", "name": "a key changed after it was bound", "base": "a webcrypto identity signs a request's key", "change": {"flip_key": true}, "expect": "bad_proof"},
        {"case": "verify", "name": "a proof changed", "base": "a webcrypto identity signs a request's key", "change": {"flip_proof": true}, "expect": "bad_proof"},
        {"case": "verify", "name": "a binding for another request", "base": "a webcrypto identity signs a request's key", "change": {"request_id": "another"}, "expect": "bad_proof"},
        {"case": "verify", "name": "a binding for the other method", "base": "a webcrypto identity signs a request's key", "change": {"method": "show"}, "expect": "bad_proof"},
        {"case": "verify", "name": "a binding that ended", "base": "a webcrypto identity signs a request's key", "change": {"now": NOW + LIFETIME + 1}, "expect": "expired"},
        {"case": "verify", "name": "a binding that lasts too long", "base": "a webcrypto identity signs a request's key", "change": {"max_lifetime": 299}, "expect": "expired"},
        {"case": "verify", "name": "a binding method nobody has", "base": "a webcrypto identity signs a request's key", "change": {"binding_method": "rsa"}, "expect": "unsupported"},
        {"case": "verify", "name": "a key of another shape", "base": "a webcrypto identity signs a request's key", "change": {"truncate_binding_key": true}, "expect": "key"},
        {"case": "verify", "name": "a noise proof changed", "base": "a noise identity proves a show's key by Diffie-Hellman", "change": {"flip_proof": true}, "expect": "bad_proof"},
        {"case": "verify", "name": "a noise proof for another request", "base": "a noise identity proves a show's key by Diffie-Hellman", "change": {"request_id": "another"}, "expect": "bad_proof"},
        {"case": "verify", "name": "a noise proof for another collector", "base": "a noise identity proves a show's key by Diffie-Hellman", "change": {"collector_private": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"}, "expect": "wrong_audience"},
        {"case": "verify", "name": "a collector with no identity of its own", "base": "a noise identity proves a show's key by Diffie-Hellman", "change": {"no_own_identity": true}, "expect": "unsupported"},
        {"case": "open", "name": "a ciphertext moved to another field", "base": "a webcrypto identity signs a request's key", "field": "password", "change": {"as_field": "token"}, "expect": "open"},
        {"case": "open", "name": "a ciphertext moved to another request", "base": "a webcrypto identity signs a request's key", "field": "password", "change": {"request_id": "another"}, "expect": "open"},
        {"case": "open", "name": "a ciphertext of the other method", "base": "a webcrypto identity signs a request's key", "field": "password", "change": {"method": "show"}, "expect": "open"},
        {"case": "open", "name": "a ciphertext changed", "base": "a webcrypto identity signs a request's key", "field": "password", "change": {"flip_ct": true}, "expect": "open"},
        {"case": "open", "name": "an encapsulated key changed", "base": "a webcrypto identity signs a request's key", "field": "password", "change": {"flip_enc": true}, "expect": "open"},
        {"case": "open", "name": "a ciphertext for another receiver", "base": "a webcrypto identity signs a request's key", "field": "password", "change": {"other_receiver": true}, "expect": "open"},
        {"case": "open", "name": "a value that does not fit its question", "base": "a webcrypto identity signs a request's key", "field": "pin", "change": {"seal_value": "not a number"}, "expect": "field"}
    ])
}

fn flipped(mut bytes: Vec<u8>) -> Vec<u8> {
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    bytes
}

fn find<'a>(items: &'a Value, name: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("no case {name}"))
}

fn method_of(text: &str) -> Method {
    if text == "show" {
        Method::Show
    } else {
        Method::Request
    }
}

/// A binding case of the vectors, rebuilt for a verification with `change` applied.
fn verify_case(doc: &Value, case: &Value) -> Result<IdentityId, Error> {
    let base = find(&doc["bindings"], case["base"].as_str().unwrap());
    let change = &case["change"];
    let mut recipient = Recipient::from_value(&base["recipient"]).unwrap();
    let identity: IdentityId = base["identity"].as_str().unwrap().parse().unwrap();
    let mut builder = Verifier::builder();
    if change["trusted"] != false {
        builder = builder.trust(identity);
    }
    let own = match (
        base["collector_private"].as_str(),
        change["collector_private"].as_str(),
        change["no_own_identity"].as_bool(),
    ) {
        (_, _, Some(true)) | (None, None, _) => None,
        (_, Some(other), _) => Some(other.to_string()),
        (Some(own), None, _) => Some(own.to_string()),
    };
    if let Some(own) = own {
        builder = builder.x25519_identity(&hex(&own)).unwrap();
    }
    if let Some(seconds) = change["max_lifetime"].as_u64() {
        builder = builder.max_lifetime(Duration::from_secs(seconds));
    }
    if change["flip_key"] == true {
        recipient.key = flipped(recipient.key.to_vec()).try_into().unwrap();
    }
    if change["flip_proof"] == true {
        recipient.binding.proof = flipped(recipient.binding.proof.clone());
    }
    if let Some(method) = change["binding_method"].as_str() {
        recipient.binding.method = method.into();
    }
    if change["truncate_binding_key"] == true {
        recipient.binding.key.truncate(10);
    }
    let request_id = change["request_id"]
        .as_str()
        .unwrap_or_else(|| base["request_id"].as_str().unwrap());
    let method = change["method"]
        .as_str()
        .unwrap_or_else(|| base["method"].as_str().unwrap());
    let now = change["now"]
        .as_u64()
        .unwrap_or_else(|| base["now"].as_u64().unwrap());
    builder.build().verify(
        method_of(method),
        request_id,
        &recipient.key,
        &recipient.binding,
        now,
    )
}

fn open_case(doc: &Value, case: &Value) -> Result<(), Error> {
    let sealed_case = find(&doc["sealed"], case["base"].as_str().unwrap());
    let base = find(&doc["bindings"], case["base"].as_str().unwrap());
    let change = &case["change"];
    let name = case["field"].as_str().unwrap();
    let form = Form::parse(&sealed_case["schema"], &Limits::new()).unwrap();
    let prover = prover_of(base);
    let request_id = base["request_id"].as_str().unwrap();
    let method = method_of(base["method"].as_str().unwrap());
    let ikm_r = if change["other_receiver"] == true {
        ikm(100)
    } else {
        hex(base["recipient_ikm"].as_str().unwrap())
    };
    let (private, public) = key_pair(&ikm_r);
    let receiver = Receiver::from_key_pair(
        method,
        request_id.into(),
        &form,
        &prover,
        NOW,
        Duration::from_secs(LIFETIME),
        private,
        &public,
    )
    .unwrap();
    let field = sealed_case["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["field"] == name)
        .unwrap();
    let mut sealed = Sealed::from_value(&field["sealed"]).unwrap();
    if change["flip_ct"] == true {
        sealed.ct = flipped(sealed.ct);
    }
    if change["flip_enc"] == true {
        sealed.enc = flipped(sealed.enc.to_vec()).try_into().unwrap();
    }
    if let Some(value) = change["seal_value"].as_str() {
        let base_recipient = Recipient::from_value(&base["recipient"]).unwrap();
        sealed = seal_with(
            &base_recipient.key,
            method,
            request_id,
            name,
            value.as_bytes(),
            &ikm(32),
        );
    }
    let opened_as = change["as_field"].as_str().unwrap_or(name);
    if change["request_id"].is_string() || change["method"].is_string() {
        // The receiver is for another request: the ciphertext must not open there.
        let other = Receiver::from_key_pair(
            method_of(
                change["method"]
                    .as_str()
                    .unwrap_or(base["method"].as_str().unwrap()),
            ),
            change["request_id"].as_str().unwrap_or(request_id).into(),
            &form,
            &prover,
            NOW,
            Duration::from_secs(LIFETIME),
            key_pair(&ikm_r).0,
            &key_pair(&ikm_r).1,
        )
        .unwrap();
        return other.open(opened_as, &sealed).map(|_| ());
    }
    receiver.open(opened_as, &sealed).map(|_| ())
}

fn prover_of(base: &Value) -> Prover {
    let private = hex(base["private"].as_str().unwrap());
    match base["kind"].as_str().unwrap() {
        "ecdsa" => Prover::ecdsa_p256(&private).unwrap(),
        _ => {
            let collector = hex(base["collector_private"].as_str().unwrap());
            Prover::x25519(&private, &x25519_spki(&x25519_public(&collector))).unwrap()
        }
    }
}

/// Everything the vectors say, checked from the file alone.
fn check(doc: &Value) {
    // RFC 9180: the key schedule, the encapsulation and the AEAD of the suite, through hpke with the RFC's ephemeral key.
    let rfc = &doc["hpke"];
    let (sk_r, pk_r) = key_pair(&hex(rfc["ikmR"].as_str().unwrap()));
    assert_eq!(
        HEXLOWER.encode(&pk_r.to_bytes()),
        rfc["pkRm"].as_str().unwrap()
    );
    let (_, pk_e) = key_pair(&hex(rfc["ikmE"].as_str().unwrap()));
    assert_eq!(
        HEXLOWER.encode(&pk_e.to_bytes()),
        rfc["pkEm"].as_str().unwrap()
    );
    let first = &rfc["encryptions"][0];
    assert_eq!(first["seq"], 0);
    let aad = hex(first["aad"].as_str().unwrap());
    let info = hex(rfc["info"].as_str().unwrap());
    let pk: [u8; 32] = pk_r.to_bytes().as_slice().try_into().unwrap();
    let made = seal_raw(
        &pk,
        &info,
        &hex(first["pt"].as_str().unwrap()),
        &aad,
        &hex(rfc["ikmE"].as_str().unwrap()),
    );
    assert_eq!(HEXLOWER.encode(&made.enc), rfc["enc"].as_str().unwrap());
    assert_eq!(HEXLOWER.encode(&made.ct), first["ct"].as_str().unwrap());
    assert_eq!(
        open_raw(&sk_r, &info, &aad, &made).unwrap(),
        hex(first["pt"].as_str().unwrap())
    );
    assert!(
        open_raw(&sk_r, &info, b"Count-1", &made).is_err(),
        "the aad is bound"
    );

    for item in doc["info"].as_array().unwrap() {
        let info = field_info(
            method_of(item["method"].as_str().unwrap()),
            item["request_id"].as_str().unwrap(),
            item["field"].as_str().unwrap(),
        );
        assert_eq!(HEXLOWER.encode(&info), item["info"].as_str().unwrap());
    }

    for base in doc["bindings"].as_array().unwrap() {
        let recipient = Recipient::from_value(&base["recipient"]).unwrap();
        let method = method_of(base["method"].as_str().unwrap());
        let request_id = base["request_id"].as_str().unwrap();
        assert_eq!(
            recipient.to_value(),
            base["recipient"],
            "a recipient reads back as it was written"
        );
        let signed = statement(
            method,
            request_id,
            &recipient.key,
            recipient.binding.expires,
            recipient.binding.audience.as_ref(),
        );
        assert_eq!(
            HEXLOWER.encode(&signed),
            base["statement"].as_str().unwrap()
        );
        let prover = prover_of(base);
        assert_eq!(prover.id().to_string(), base["identity"].as_str().unwrap());
        let case = json!({"base": base["name"], "change": {}});
        assert_eq!(verify_case(doc, &case).unwrap(), prover.id());
        // The same bytes from the same inputs: the signature is deterministic, the Diffie-Hellman proof always is.
        let (private, public) = key_pair(&hex(base["recipient_ikm"].as_str().unwrap()));
        let remade = Receiver::from_key_pair(
            method,
            request_id.into(),
            &form(),
            &prover,
            NOW,
            Duration::from_secs(LIFETIME),
            private,
            &public,
        )
        .unwrap();
        assert_eq!(remade.recipient().to_value(), base["recipient"]);
    }

    for entry in doc["sealed"].as_array().unwrap() {
        let base = find(&doc["bindings"], entry["binding"].as_str().unwrap());
        let form = Form::parse(&entry["schema"], &Limits::new()).unwrap();
        let method = method_of(base["method"].as_str().unwrap());
        let request_id = base["request_id"].as_str().unwrap();
        let (private, public) = key_pair(&hex(base["recipient_ikm"].as_str().unwrap()));
        let receiver = Receiver::from_key_pair(
            method,
            request_id.into(),
            &form,
            &prover_of(base),
            NOW,
            Duration::from_secs(LIFETIME),
            private,
            &public,
        )
        .unwrap();
        let verifier = Verifier::builder()
            .trust(base["identity"].as_str().unwrap().parse().unwrap())
            .x25519_identity(&hex(base["collector_private"]
                .as_str()
                .unwrap_or(NOISE_COLLECTOR)))
            .unwrap()
            .build();
        let collector = verifier
            .collector(method, request_id, &form, receiver.recipient(), NOW)
            .unwrap();
        for field in entry["fields"].as_array().unwrap() {
            let name = field["field"].as_str().unwrap();
            let value = field["value"].as_str().unwrap();
            let sealed = Sealed::from_value(&field["sealed"]).unwrap();
            assert_eq!(
                receiver.open(name, &sealed).unwrap().expose_str(),
                value,
                "{name}"
            );
            // A fresh sealing opens too, though its bytes differ with the ephemeral key.
            let fresh = collector.seal(name, value).unwrap();
            assert_eq!(receiver.open(name, &fresh).unwrap().expose_str(), value);
            assert_ne!(fresh, sealed);
        }
    }

    for item in doc["forms"]["accepted"].as_array().unwrap() {
        let form = Form::parse(&item["schema"], &Limits::new())
            .unwrap_or_else(|error| panic!("{}: {error}", item["name"]));
        let names = |secret: bool| -> Vec<String> {
            form.fields
                .iter()
                .filter(|field| field.secret == secret)
                .map(|field| field.field.name.clone())
                .collect()
        };
        assert_eq!(
            names(true),
            item["secret"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            names(false),
            item["plain"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        );
    }
    for item in doc["forms"]["refused"].as_array().unwrap() {
        let error =
            Form::parse(&item["schema"], &Limits::new()).expect_err(item["name"].as_str().unwrap());
        assert_eq!(
            kind(&error),
            item["kind"].as_str().unwrap(),
            "{}",
            item["name"]
        );
    }

    for item in doc["refusals"].as_array().unwrap() {
        let error = match item["case"].as_str().unwrap() {
            "verify" => verify_case(doc, item).expect_err(item["name"].as_str().unwrap()),
            _ => open_case(doc, item).expect_err(item["name"].as_str().unwrap()),
        };
        assert_eq!(
            kind(&error),
            item["expect"].as_str().unwrap(),
            "{}: {error}",
            item["name"]
        );
    }
}

#[test]
fn vectors_hold() {
    check(&serde_json::from_str(VECTORS).expect("vectors/secret.json is JSON"));
}

/// Writes `vectors/secret.json` when `COQUI_ACP_WRITE_VECTORS` is set; otherwise says the file is what this code makes.
#[test]
fn vectors_are_current() {
    let mut doc: Map<String, Value> =
        serde_json::from_str(VECTORS).expect("vectors/secret.json is JSON");
    let made = generate();
    if std::env::var_os("COQUI_ACP_WRITE_VECTORS").is_some() {
        for (key, value) in made {
            doc.insert(key, value);
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../vectors/secret.json");
        let mut text = serde_json::to_string_pretty(&Value::Object(doc)).unwrap();
        text.push('\n');
        std::fs::write(path, text).unwrap();
        return;
    }
    for (key, value) in made {
        assert_eq!(
            doc.get(&key),
            Some(&value),
            "{key} differs from what the code makes; rerun with COQUI_ACP_WRITE_VECTORS=1"
        );
    }
}
