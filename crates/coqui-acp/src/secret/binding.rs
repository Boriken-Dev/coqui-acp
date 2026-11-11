//! Binding a per-request key to the identity that holds it, so a collector seals only to a key a peer it already knows stands behind.
//!
//! A binding proves, for one request, that the holder of an identity made the key. Two proofs cover the peer identity methods:
//! an ECDSA signature (P-256, SHA-256, IEEE P1363) by an identity that signs, and a Diffie-Hellman proof (HMAC under a key derived
//! from the two X25519 static keys) by one that cannot, such as a Noise static key.

use super::error::Error;
use super::{b64u, b64u_decode};
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;
use core::time::Duration;
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use p256::ecdsa::signature::{Signer, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::DecodePrivateKey;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};
use zeroize::Zeroizing;

/// The binding method for an identity that signs: ECDSA over P-256 and SHA-256, the signature in IEEE P1363 form (`r` then `s`, 64
/// bytes), as the `webcrypto` peer method signs. The key is the identity's SPKI.
pub const BINDING_ECDSA: &str = "ecdsa-p256-sha256";
/// The binding method for an X25519 identity: HMAC-SHA-256 under a key derived from the Diffie-Hellman of the holder's static key
/// and the collector's, so only those two can make it. The key is the identity's SPKI.
pub const BINDING_DH: &str = "x25519-hmac-sha256";

const STATEMENT_LABEL: &[u8] = b"dev.boriken.coqui.acp/secret/v1 binding";
const DH_LABEL: &[u8] = b"dev.boriken.coqui.acp/secret/v1 dh";

/// `SubjectPublicKeyInfo` of a P-256 key before its 65-byte uncompressed point.
const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];
/// `SubjectPublicKeyInfo` of an X25519 key before its 32 bytes (RFC 8410).
const X25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
];
/// The PKCS#8 `PrivateKeyInfo` of an X25519 key before its 32 bytes (RFC 8410).
const X25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04, 0x20,
];

/// An identity as the peer methods name it: the SHA-256 of its public key's encoding (its `SubjectPublicKeyInfo`), written
/// `sha256:` and 64 lower-case hexadecimal digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdentityId([u8; 32]);

impl IdentityId {
    /// The id of the public key whose encoding is `spki`.
    #[must_use]
    pub fn of(spki: &[u8]) -> Self {
        Self(Sha256::digest(spki).into())
    }

    /// The id as its 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for IdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", data_encoding::HEXLOWER.encode(&self.0))
    }
}

impl fmt::Debug for IdentityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IdentityId({self})")
    }
}

impl FromStr for IdentityId {
    type Err = Error;

    /// Reads `sha256:` and 64 hexadecimal digits, either case.
    fn from_str(text: &str) -> Result<Self, Error> {
        let hex = text.strip_prefix("sha256:").ok_or(Error::Malformed(
            "an identity is sha256: and 64 hexadecimal digits",
        ))?;
        let bytes = data_encoding::HEXLOWER_PERMISSIVE
            .decode(hex.as_bytes())
            .map_err(|_| Error::Malformed("an identity is sha256: and 64 hexadecimal digits"))?;
        Ok(Self(bytes.try_into().map_err(|_| {
            Error::Malformed("an identity is sha256: and 64 hexadecimal digits")
        })?))
    }
}

/// Which of the extension's two methods a request is. It is bound into every sealed field and every key binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// The person is asked for secrets: the host receives them.
    Request,
    /// The person is shown secrets: the frontend receives them.
    Show,
}

impl Method {
    /// `request` or `show`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Show => "show",
        }
    }
}

/// A key binding as it travels.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Binding {
    /// [`BINDING_ECDSA`] or [`BINDING_DH`]; any other is refused.
    pub method: String,
    /// The holder's public key as `SubjectPublicKeyInfo`. Its [`IdentityId`] is what a collector looks up.
    pub key: Vec<u8>,
    /// When the binding ends, in Unix seconds.
    pub expires: u64,
    /// The collector it was made for, when the maker knew one: required by [`BINDING_DH`].
    pub audience: Option<IdentityId>,
    /// The signature or the tag.
    pub proof: Vec<u8>,
}

impl Binding {
    /// The binding as JSON: `{"method", "key", "expires", "audience"?, "proof"}`, bytes in unpadded base64url.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("method".into(), Value::String(self.method.clone()));
        map.insert("key".into(), Value::String(b64u(&self.key)));
        map.insert("expires".into(), Value::from(self.expires));
        if let Some(audience) = &self.audience {
            map.insert(
                "audience".into(),
                Value::String(alloc::format!("{audience}")),
            );
        }
        map.insert("proof".into(), Value::String(b64u(&self.proof)));
        Value::Object(map)
    }

    /// Reads a binding.
    ///
    /// # Errors
    /// [`Error::Malformed`] for anything not in the shape `to_value` writes.
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        let map = value
            .as_object()
            .ok_or(Error::Malformed("a binding is not an object"))?;
        let text = |key: &str| map.get(key).and_then(Value::as_str);
        Ok(Self {
            method: text("method")
                .ok_or(Error::Malformed("a binding has no method"))?
                .into(),
            key: b64u_decode(text("key").ok_or(Error::Malformed("a binding has no key"))?)?,
            expires: map
                .get("expires")
                .and_then(Value::as_u64)
                .ok_or(Error::Malformed("a binding has no end"))?,
            audience: text_audience(map)?,
            proof: b64u_decode(text("proof").ok_or(Error::Malformed("a binding has no proof"))?)?,
        })
    }
}

fn text_audience(map: &Map<String, Value>) -> Result<Option<IdentityId>, Error> {
    match map.get("audience") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => text.parse().map(Some),
        Some(_) => Err(Error::Malformed("a binding's audience is not text")),
    }
}

fn push(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The bytes a binding proves: the label, the method, the request, the key, the end and the collector, each with a 4-byte length.
pub(crate) fn statement(
    method: Method,
    request_id: &str,
    key: &[u8; 32],
    expires: u64,
    audience: Option<&IdentityId>,
) -> Vec<u8> {
    let mut out = Vec::new();
    push(&mut out, STATEMENT_LABEL);
    push(&mut out, method.as_str().as_bytes());
    push(&mut out, request_id.as_bytes());
    push(&mut out, key);
    push(&mut out, &expires.to_be_bytes());
    push(&mut out, audience.map_or(&[][..], |id| &id.0[..]));
    out
}

fn p256_spki(key: &VerifyingKey) -> Vec<u8> {
    let mut spki = P256_SPKI_PREFIX.to_vec();
    spki.extend_from_slice(key.to_sec1_point(false).as_bytes());
    spki
}

fn x25519_spki(public: &[u8; 32]) -> Vec<u8> {
    let mut spki = X25519_SPKI_PREFIX.to_vec();
    spki.extend_from_slice(public);
    spki
}

fn x25519_of_spki(spki: &[u8]) -> Result<[u8; 32], Error> {
    spki.strip_prefix(&X25519_SPKI_PREFIX[..])
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
        .ok_or(Error::Key("not an X25519 SubjectPublicKeyInfo"))
}

/// The MAC key two X25519 static keys share: HKDF-SHA-256 of their Diffie-Hellman, bound to both public keys, holder first.
fn dh_key(
    secret: &StaticSecret,
    other: &[u8; 32],
    holder: &[u8; 32],
    collector: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, Error> {
    let shared = secret.diffie_hellman(&X25519Public::from(*other));
    if !shared.was_contributory() {
        return Err(Error::Key("a Diffie-Hellman key of low order"));
    }
    let mut info = DH_LABEL.to_vec();
    info.extend_from_slice(holder);
    info.extend_from_slice(collector);
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, shared.as_bytes())
        .expand(&info, &mut *key)
        .map_err(|_| Error::Seal)?;
    Ok(key)
}

fn tag(key: &[u8; 32], statement: &[u8]) -> Result<Hmac<Sha256>, Error> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).map_err(|_| Error::Seal)?;
    mac.update(statement);
    Ok(mac)
}

/// The holder's side of a binding: the identity's private key, and for an X25519 identity the collector it will talk to.
pub struct Prover {
    kind: ProverKind,
}

enum ProverKind {
    Ecdsa {
        key: SigningKey,
        spki: Vec<u8>,
    },
    Dh {
        secret: StaticSecret,
        spki: Vec<u8>,
        collector: [u8; 32],
        audience: IdentityId,
    },
}

impl fmt::Debug for Prover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Prover({})", self.id())
    }
}

impl Prover {
    /// An identity that signs, from its P-256 private scalar (32 bytes).
    ///
    /// # Errors
    /// [`Error::Key`] for a scalar that is no key.
    pub fn ecdsa_p256(scalar: &[u8]) -> Result<Self, Error> {
        let key =
            SigningKey::from_slice(scalar).map_err(|_| Error::Key("not a P-256 private scalar"))?;
        let spki = p256_spki(key.verifying_key());
        Ok(Self {
            kind: ProverKind::Ecdsa { key, spki },
        })
    }

    /// An identity that signs, from its PKCS#8 private key (`-----BEGIN PRIVATE KEY-----`'s bytes), as the `webcrypto` peer method
    /// writes it.
    ///
    /// # Errors
    /// [`Error::Key`] for what is not a P-256 PKCS#8 key.
    pub fn ecdsa_p256_pkcs8(der: &[u8]) -> Result<Self, Error> {
        let key =
            SigningKey::from_pkcs8_der(der).map_err(|_| Error::Key("not a P-256 PKCS#8 key"))?;
        let spki = p256_spki(key.verifying_key());
        Ok(Self {
            kind: ProverKind::Ecdsa { key, spki },
        })
    }

    /// An X25519 identity, from its private key (32 bytes), for the collector whose `SubjectPublicKeyInfo` is `collector_spki`
    /// (the app's identity, which the holder knows by pairing).
    ///
    /// # Errors
    /// [`Error::Key`] for a key of another length, or a collector that is no X25519 key.
    pub fn x25519(secret: &[u8], collector_spki: &[u8]) -> Result<Self, Error> {
        let secret: [u8; 32] = secret
            .try_into()
            .map_err(|_| Error::Key("an X25519 private key is 32 bytes"))?;
        let secret = StaticSecret::from(secret);
        let collector = x25519_of_spki(collector_spki)?;
        let spki = x25519_spki(X25519Public::from(&secret).as_bytes());
        Ok(Self {
            kind: ProverKind::Dh {
                secret,
                spki,
                collector,
                audience: IdentityId::of(collector_spki),
            },
        })
    }

    /// An X25519 identity from its PKCS#8 private key, as the `noise` peer method writes it (RFC 8410, 48 bytes).
    ///
    /// # Errors
    /// [`Error::Key`].
    pub fn x25519_pkcs8(der: &[u8], collector_spki: &[u8]) -> Result<Self, Error> {
        let key = der
            .strip_prefix(&X25519_PKCS8_PREFIX[..])
            .filter(|key| key.len() == 32)
            .ok_or(Error::Key("not an X25519 PKCS#8 key"))?;
        Self::x25519(key, collector_spki)
    }

    /// The identity's public key as `SubjectPublicKeyInfo`.
    #[must_use]
    pub fn spki(&self) -> &[u8] {
        match &self.kind {
            ProverKind::Ecdsa { spki, .. } | ProverKind::Dh { spki, .. } => spki,
        }
    }

    /// The identity's id.
    #[must_use]
    pub fn id(&self) -> IdentityId {
        IdentityId::of(self.spki())
    }

    /// Binds `key`, the receiver's per-request public key, to this identity for one request, until `expires`.
    pub(crate) fn bind(
        &self,
        method: Method,
        request_id: &str,
        key: &[u8; 32],
        expires: u64,
    ) -> Result<Binding, Error> {
        match &self.kind {
            ProverKind::Ecdsa { key: signing, spki } => {
                let signed: Signature =
                    signing.sign(&statement(method, request_id, key, expires, None));
                // Low s, so one request has one signature in every language; a verifier takes either.
                let proof = signed.normalize_s();
                Ok(Binding {
                    method: BINDING_ECDSA.into(),
                    key: spki.clone(),
                    expires,
                    audience: None,
                    proof: proof.to_bytes().to_vec(),
                })
            }
            ProverKind::Dh {
                secret,
                spki,
                collector,
                audience,
            } => {
                let holder = x25519_of_spki(spki)?;
                let mac = dh_key(secret, collector, &holder, collector)?;
                let proof = tag(
                    &mac,
                    &statement(method, request_id, key, expires, Some(audience)),
                )?
                .finalize()
                .into_bytes();
                Ok(Binding {
                    method: BINDING_DH.into(),
                    key: spki.clone(),
                    expires,
                    audience: Some(*audience),
                    proof: proof.to_vec(),
                })
            }
        }
    }
}

/// The collector's side: the identities it knows by pairing or pinning, and its own X25519 key when it takes [`BINDING_DH`].
#[derive(Debug)]
pub struct Verifier {
    pub(crate) known: BTreeSet<IdentityId>,
    own: Option<Own>,
    max_lifetime: Duration,
}

struct Own {
    secret: StaticSecret,
    id: IdentityId,
    public: [u8; 32],
}

impl fmt::Debug for Own {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Own({})", self.id)
    }
}

/// Builds a [`Verifier`].
#[derive(Debug)]
#[must_use]
pub struct VerifierBuilder {
    known: BTreeSet<IdentityId>,
    own: Option<Own>,
    max_lifetime: Duration,
}

impl VerifierBuilder {
    /// Knows an identity by its id: one the person paired or pinned. A key whose holder is not known is refused, whatever it carries.
    pub fn trust(mut self, id: IdentityId) -> Self {
        self.known.insert(id);
        self
    }

    /// Knows an identity by its `SubjectPublicKeyInfo`.
    pub fn trust_spki(self, spki: &[u8]) -> Self {
        self.trust(IdentityId::of(spki))
    }

    /// Gives the collector's own X25519 identity, which [`BINDING_DH`] needs: its private key (32 bytes). Without it that method is
    /// refused.
    ///
    /// # Errors
    /// [`Error::Key`] for a key of another length.
    pub fn x25519_identity(mut self, secret: &[u8]) -> Result<Self, Error> {
        let secret: [u8; 32] = secret
            .try_into()
            .map_err(|_| Error::Key("an X25519 private key is 32 bytes"))?;
        let secret = StaticSecret::from(secret);
        let public = *X25519Public::from(&secret).as_bytes();
        self.own = Some(Own {
            id: IdentityId::of(&x25519_spki(&public)),
            public,
            secret,
        });
        Ok(self)
    }

    /// The longest a binding may last from now, 5 minutes by default; any value is taken.
    pub const fn max_lifetime(mut self, lifetime: Duration) -> Self {
        self.max_lifetime = lifetime;
        self
    }

    /// Builds the collector's verifier.
    #[must_use]
    pub fn build(self) -> Verifier {
        Verifier {
            known: self.known,
            own: self.own,
            max_lifetime: self.max_lifetime,
        }
    }
}

impl Verifier {
    /// Starts a verifier that knows no one.
    pub fn builder() -> VerifierBuilder {
        VerifierBuilder {
            known: BTreeSet::new(),
            own: None,
            max_lifetime: Duration::from_secs(300),
        }
    }

    /// Checks that `binding` binds `key` to a known identity for this request, at `now` (Unix seconds); returns that identity.
    ///
    /// The order is: the method and the key's shape, whether the holder is known, the end, the collector, and last the proof, so a
    /// peer nobody knows costs one hash.
    ///
    /// # Errors
    /// [`Error::Unsupported`], [`Error::Key`], [`Error::UnknownIdentity`], [`Error::Expired`], [`Error::WrongAudience`],
    /// [`Error::BadProof`].
    pub(crate) fn verify(
        &self,
        method: Method,
        request_id: &str,
        key: &[u8; 32],
        binding: &Binding,
        now: u64,
    ) -> Result<IdentityId, Error> {
        let is_ecdsa = match binding.method.as_str() {
            BINDING_ECDSA => true,
            BINDING_DH => false,
            _ => {
                return Err(Error::Unsupported(
                    "a binding method this side does not have",
                ));
            }
        };
        if is_ecdsa {
            if binding.key.len() != 91 || !binding.key.starts_with(&P256_SPKI_PREFIX) {
                return Err(Error::Key("not a P-256 SubjectPublicKeyInfo"));
            }
        } else {
            x25519_of_spki(&binding.key)?;
        }
        let holder = IdentityId::of(&binding.key);
        if !self.known.contains(&holder) {
            return Err(Error::UnknownIdentity);
        }
        if now > binding.expires
            || binding.expires > now.saturating_add(self.max_lifetime.as_secs())
        {
            return Err(Error::Expired);
        }
        if let (Some(audience), Some(own)) = (&binding.audience, &self.own)
            && *audience != own.id
        {
            return Err(Error::WrongAudience);
        }
        let signed = statement(
            method,
            request_id,
            key,
            binding.expires,
            binding.audience.as_ref(),
        );
        if is_ecdsa {
            let verifying = VerifyingKey::from_sec1_bytes(&binding.key[P256_SPKI_PREFIX.len()..])
                .map_err(|_| Error::Key("not a P-256 point"))?;
            let signature = Signature::from_slice(&binding.proof).map_err(|_| Error::BadProof)?;
            verifying
                .verify(&signed, &signature)
                .map_err(|_| Error::BadProof)?;
        } else {
            let own = self.own.as_ref().ok_or(Error::Unsupported(
                "this collector has no X25519 identity of its own",
            ))?;
            if binding.audience != Some(own.id) {
                return Err(Error::WrongAudience);
            }
            let holder_key = x25519_of_spki(&binding.key)?;
            let mac = dh_key(&own.secret, &holder_key, &holder_key, &own.public)?;
            tag(&mac, &signed)?
                .verify_slice(&binding.proof)
                .map_err(|_| Error::BadProof)?;
        }
        Ok(holder)
    }
}
