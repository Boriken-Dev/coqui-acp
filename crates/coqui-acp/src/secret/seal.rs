//! Sealing a secret field to a per-request key, and opening it, by HPKE (RFC 9180) in base mode with DHKEM(X25519, HKDF-SHA256),
//! HKDF-SHA256 and AES-128-GCM. The `info` of each field names the extension, the method, the request and the field, so a
//! ciphertext opens only for the field and request it was made for.

use super::binding::{Binding, Method, Prover, Verifier};
use super::error::Error;
use super::form::Form;
use super::{b64u, b64u_decode};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;
use hpke::aead::{AeadTag, AesGcm128};
use hpke::inout::InOutBuf;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable};
use serde_json::{Map, Value};
use zeroize::Zeroizing;

type Kem = X25519HkdfSha256;
type PrivateKey = <Kem as KemTrait>::PrivateKey;
type PublicKey = <Kem as KemTrait>::PublicKey;
type EncappedKey = <Kem as KemTrait>::EncappedKey;

const INFO_LABEL: &[u8] = b"dev.boriken.coqui.acp/secret/v1 field";
const TAG_BYTES: usize = 16;

/// A sealed field as it travels: HPKE's encapsulated key and the ciphertext with its tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    /// The encapsulated key, 32 bytes.
    pub enc: [u8; 32],
    /// The ciphertext followed by its 16-byte tag.
    pub ct: Vec<u8>,
}

impl Sealed {
    /// `{"enc", "ct"}`, bytes in unpadded base64url.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("enc".into(), Value::String(b64u(&self.enc)));
        map.insert("ct".into(), Value::String(b64u(&self.ct)));
        Value::Object(map)
    }

    /// Reads a sealed field.
    ///
    /// # Errors
    /// [`Error::Malformed`] for anything else.
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        let map = value
            .as_object()
            .filter(|map| map.len() == 2)
            .ok_or(Error::Malformed("a sealed value is not {enc, ct}"))?;
        let part = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .ok_or(Error::Malformed("a sealed value is not {enc, ct}"))
                .and_then(b64u_decode)
        };
        let ct = part("ct")?;
        if ct.len() < TAG_BYTES {
            return Err(Error::Malformed("a sealed value is shorter than its tag"));
        }
        Ok(Self {
            enc: part("enc")?
                .try_into()
                .map_err(|_| Error::Malformed("a sealed value's key is not 32 bytes"))?,
            ct,
        })
    }
}

/// A secret value held for its use: zeroed when dropped, in no `Debug` and no `Display`.
pub struct SecretValue(Zeroizing<Vec<u8>>);

impl SecretValue {
    /// The value as bytes, UTF-8 text for a text field and decimal text for a number.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// The value as text; it was checked to be UTF-8 when it was opened.
    #[must_use]
    pub fn expose_str(&self) -> &str {
        core::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(..)")
    }
}

/// The receiver's per-request public key with its binding: what the collector is sent, and checks, before it seals.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Recipient {
    /// The X25519 public key the fields are sealed to.
    pub key: [u8; 32],
    /// Why the collector may believe the key is the receiver's.
    pub binding: Binding,
}

impl Recipient {
    /// `{"key", "binding"}`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("key".into(), Value::String(b64u(&self.key)));
        map.insert("binding".into(), self.binding.to_value());
        Value::Object(map)
    }

    /// Reads a recipient.
    ///
    /// # Errors
    /// [`Error::Malformed`].
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        let map = value
            .as_object()
            .ok_or(Error::Malformed("a recipient is not an object"))?;
        let key = map
            .get("key")
            .and_then(Value::as_str)
            .ok_or(Error::Malformed("a recipient has no key"))?;
        Ok(Self {
            key: b64u_decode(key)?
                .try_into()
                .map_err(|_| Error::Malformed("a recipient's key is not 32 bytes"))?,
            binding: Binding::from_value(
                map.get("binding")
                    .ok_or(Error::Malformed("a recipient has no binding"))?,
            )?,
        })
    }
}

/// What the receiver gets back: the plain answers, and each secret one opened.
#[derive(Debug)]
#[non_exhaustive]
pub struct Opened {
    /// The plain questions' answers, as the form allows them.
    pub plain: Map<String, Value>,
    /// The secret questions' values.
    pub secrets: BTreeMap<String, SecretValue>,
}

/// The `info` of one field: the extension, the method, the request and the field, each with a 4-byte length.
fn info(method: Method, request_id: &str, field: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for part in [
        INFO_LABEL,
        method.as_str().as_bytes(),
        request_id.as_bytes(),
        field.as_bytes(),
    ] {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part);
    }
    out
}

fn seal_buffer(
    value: &[u8],
    seal: impl FnOnce(&mut [u8]) -> Result<(EncappedKey, AeadTag<AesGcm128>), Error>,
) -> Result<Sealed, Error> {
    let mut buffer = Zeroizing::new(value.to_vec());
    let (enc, tag) = seal(&mut buffer)?;
    let mut ct = buffer.to_vec();
    ct.extend_from_slice(tag.to_bytes().as_slice());
    Ok(Sealed {
        enc: enc
            .to_bytes()
            .as_slice()
            .try_into()
            .map_err(|_| Error::Seal)?,
        ct,
    })
}

/// Seals `value` to `key` for `info`, from the system's randomness.
fn seal(key: &[u8; 32], info: &[u8], value: &[u8]) -> Result<Sealed, Error> {
    let pk = PublicKey::from_bytes(key).map_err(|_| Error::Key("not an X25519 public key"))?;
    seal_buffer(value, |buffer| {
        hpke::single_shot_seal_inout_detached::<AesGcm128, HkdfSha256, Kem>(
            &OpModeS::Base,
            &pk,
            info,
            InOutBuf::from(buffer),
            &[],
        )
        .map_err(|_| Error::Seal)
    })
}

/// Opens `sealed` with `private` for `info`.
fn open(private: &PrivateKey, info: &[u8], sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, Error> {
    let enc = EncappedKey::from_bytes(&sealed.enc).map_err(|_| Error::Open)?;
    let split = sealed.ct.len().checked_sub(TAG_BYTES).ok_or(Error::Open)?;
    let tag = AeadTag::<AesGcm128>::from_bytes(&sealed.ct[split..]).map_err(|_| Error::Open)?;
    let mut buffer = Zeroizing::new(sealed.ct[..split].to_vec());
    hpke::single_shot_open_inout_detached::<AesGcm128, HkdfSha256, Kem>(
        &OpModeR::Base,
        private,
        &enc,
        info,
        InOutBuf::from(&mut buffer[..]),
        &[],
        &tag,
    )
    .map_err(|_| Error::Open)?;
    Ok(buffer)
}

/// The receiving side of one request: it holds the per-request private key, and gives the collector the public key with a binding.
/// The host is the receiver of a `request`, the frontend of a `show`.
pub struct Receiver {
    method: Method,
    request_id: String,
    form: Form,
    private: PrivateKey,
    recipient: Recipient,
}

impl fmt::Debug for Receiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver")
            .field("method", &self.method)
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl Receiver {
    /// Makes a key pair for one request and binds the public key to `prover`'s identity until `now` plus `lifetime` (Unix seconds).
    ///
    /// # Errors
    /// [`Error::Malformed`] for an empty or over-long request id; [`Error::Seal`] when the binding cannot be made.
    pub fn new(
        method: Method,
        request_id: impl Into<String>,
        form: &Form,
        prover: &Prover,
        now: u64,
        lifetime: Duration,
    ) -> Result<Self, Error> {
        let (private, public) = Kem::gen_keypair();
        Self::from_key_pair(
            method,
            request_id.into(),
            form,
            prover,
            now,
            lifetime,
            private,
            &public,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_key_pair(
        method: Method,
        request_id: String,
        form: &Form,
        prover: &Prover,
        now: u64,
        lifetime: Duration,
        private: PrivateKey,
        public: &PublicKey,
    ) -> Result<Self, Error> {
        check_id(&request_id)?;
        let key: [u8; 32] = public
            .to_bytes()
            .as_slice()
            .try_into()
            .map_err(|_| Error::Seal)?;
        let binding = prover.bind(
            method,
            &request_id,
            &key,
            now.saturating_add(lifetime.as_secs()),
        )?;
        Ok(Self {
            method,
            request_id,
            form: form.clone(),
            private,
            recipient: Recipient { key, binding },
        })
    }

    /// What to send the collector.
    #[must_use]
    pub const fn recipient(&self) -> &Recipient {
        &self.recipient
    }

    /// The request this receiver is for.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Opens one secret field.
    ///
    /// # Errors
    /// [`Error::Field`] for a name that is no secret question; [`Error::Open`] for a ciphertext made for another request or field
    /// or another key, or damaged; [`Error::Field`] again for a value that does not fit its question.
    pub fn open(&self, name: &str, sealed: &Sealed) -> Result<SecretValue, Error> {
        if !self.form.field(name).is_some_and(|field| field.secret) {
            return Err(Error::Field("not a secret question of the form"));
        }
        let plain = open(
            &self.private,
            &info(self.method, &self.request_id, name),
            sealed,
        )?;
        self.form.check_secret(name, &plain)?;
        Ok(SecretValue(plain))
    }

    /// Opens an answer: checks the plain values against the form and opens every sealed one.
    ///
    /// # Errors
    /// Any of [`Receiver::open`]'s, and [`Error::Field`] for a name the form lacks, a plain value for a secret question, a sealed one
    /// for a plain question, or a required question left out.
    pub fn open_values(
        &self,
        plain: &Map<String, Value>,
        sealed: &BTreeMap<String, Sealed>,
    ) -> Result<Opened, Error> {
        self.form.check_plain(plain)?;
        self.form.check_sealed(sealed.keys().map(String::as_str))?;
        let secrets = sealed
            .iter()
            .map(|(name, value)| Ok((name.clone(), self.open(name, value)?)))
            .collect::<Result<BTreeMap<_, _>, Error>>()?;
        Ok(Opened {
            plain: plain.clone(),
            secrets,
        })
    }
}

fn check_id(request_id: &str) -> Result<(), Error> {
    if request_id.is_empty() || request_id.len() > 128 || request_id.chars().any(char::is_control) {
        return Err(Error::Malformed(
            "a request id is 1 to 128 bytes with no control character",
        ));
    }
    Ok(())
}

/// The collecting side of one request, after it checked the receiver's key: it seals each secret value to that key.
#[derive(Debug)]
pub struct Collector {
    method: Method,
    request_id: String,
    form: Form,
    key: [u8; 32],
    holder: super::binding::IdentityId,
}

impl Verifier {
    /// Checks the receiver's key and its binding at `now` (Unix seconds), and returns the collector that seals to it.
    ///
    /// # Errors
    /// The refusals of the binding ([`Error::UnknownIdentity`], [`Error::BadProof`], [`Error::Expired`], [`Error::WrongAudience`], ...).
    pub fn collector(
        &self,
        method: Method,
        request_id: &str,
        form: &Form,
        recipient: &Recipient,
        now: u64,
    ) -> Result<Collector, Error> {
        check_id(request_id)?;
        let holder = self.verify(method, request_id, &recipient.key, &recipient.binding, now)?;
        PublicKey::from_bytes(&recipient.key)
            .map_err(|_| Error::Key("not an X25519 public key"))?;
        Ok(Collector {
            method,
            request_id: request_id.into(),
            form: form.clone(),
            key: recipient.key,
            holder,
        })
    }
}

impl Collector {
    /// The identity that stands behind the key.
    #[must_use]
    pub const fn holder(&self) -> super::binding::IdentityId {
        self.holder
    }

    /// Seals the value of secret field `name`: UTF-8 text, or a number as decimal text.
    ///
    /// # Errors
    /// [`Error::Field`] for a name that is no secret question or a value that does not fit it; [`Error::Seal`].
    pub fn seal(&self, name: &str, value: &str) -> Result<Sealed, Error> {
        self.form.check_secret(name, value.as_bytes())?;
        seal(
            &self.key,
            &info(self.method, &self.request_id, name),
            value.as_bytes(),
        )
    }

    /// Whether the field is one this collector would seal, without sealing anything.
    #[must_use]
    pub fn is_secret(&self, name: &str) -> bool {
        self.form.field(name).is_some_and(|field| field.secret)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use hpke::rand_core::{TryCryptoRng, TryRng};

    /// Randomness that reads off the bytes given, as RFC 9180's vectors fix them.
    pub(crate) struct Fixed<'a>(pub(crate) &'a [u8]);

    impl TryRng for Fixed<'_> {
        type Error = core::convert::Infallible;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            unreachable!("only bytes are asked for")
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            unreachable!("only bytes are asked for")
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Self::Error> {
            let (taken, rest) = self.0.split_at(dest.len());
            dest.copy_from_slice(taken);
            self.0 = rest;
            Ok(())
        }
    }

    impl TryCryptoRng for Fixed<'_> {}

    pub(crate) fn key_pair(ikm: &[u8]) -> (PrivateKey, PublicKey) {
        Kem::derive_keypair(ikm)
    }

    pub(crate) fn seal_with(
        key: &[u8; 32],
        method: Method,
        request_id: &str,
        field: &str,
        value: &[u8],
        ikm: &[u8],
    ) -> Sealed {
        let pk = PublicKey::from_bytes(key).unwrap();
        let info = info(method, request_id, field);
        seal_buffer(value, |buffer| {
            hpke::single_shot_seal_inout_detached_with_rng::<AesGcm128, HkdfSha256, Kem>(
                &OpModeS::Base,
                &pk,
                &info,
                InOutBuf::from(buffer),
                &[],
                &mut Fixed(ikm),
            )
            .map_err(|_| Error::Seal)
        })
        .unwrap()
    }

    pub(crate) fn seal_raw(
        key: &[u8; 32],
        info: &[u8],
        value: &[u8],
        aad: &[u8],
        ikm: &[u8],
    ) -> Sealed {
        let pk = PublicKey::from_bytes(key).unwrap();
        seal_buffer(value, |buffer| {
            hpke::single_shot_seal_inout_detached_with_rng::<AesGcm128, HkdfSha256, Kem>(
                &OpModeS::Base,
                &pk,
                info,
                InOutBuf::from(buffer),
                aad,
                &mut Fixed(ikm),
            )
            .map_err(|_| Error::Seal)
        })
        .unwrap()
    }

    pub(crate) fn open_raw(
        private: &PrivateKey,
        info: &[u8],
        aad: &[u8],
        sealed: &Sealed,
    ) -> Result<Vec<u8>, Error> {
        let enc = EncappedKey::from_bytes(&sealed.enc).map_err(|_| Error::Open)?;
        let split = sealed.ct.len() - TAG_BYTES;
        let tag = AeadTag::<AesGcm128>::from_bytes(&sealed.ct[split..]).map_err(|_| Error::Open)?;
        let mut buffer = sealed.ct[..split].to_vec();
        hpke::single_shot_open_inout_detached::<AesGcm128, HkdfSha256, Kem>(
            &OpModeR::Base,
            private,
            &enc,
            info,
            InOutBuf::from(&mut buffer[..]),
            aad,
            &tag,
        )
        .map_err(|_| Error::Open)?;
        Ok(buffer)
    }

    pub(crate) fn field_info(method: Method, request_id: &str, field: &str) -> Vec<u8> {
        info(method, request_id, field)
    }
}
