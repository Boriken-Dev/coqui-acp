//! Proving a peer before any byte reaches the agent.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use coqui_peer::{Handshake, Method, PeerId};
use coqui_peer_webcrypto::WebCryptoMethod;
use coqui_web::{Channel, Failure};
use p256::SecretKey;
use p256::ecdsa::SigningKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rand_core::OsRng;

use crate::Error;

/// A browser's proof fits in a few short messages; this also bounds what an unproven peer can make us hold.
pub const MAX_PROOF_MESSAGE: usize = 3072;

/// Reads a P-256 PKCS#8 PEM identity for the browser-compatible method.
///
/// # Errors
///
/// [`Error::Identity`] when the file cannot be read or is not such a key. The message never quotes the file.
pub fn read_identity(path: &Path) -> Result<SigningKey, Error> {
    let text = fs::read_to_string(path).map_err(|error| Error::Identity(error.to_string()))?;
    SigningKey::from_pkcs8_pem(&text)
        .map_err(|_| Error::Identity("not a P-256 PKCS#8 PEM key".to_owned()))
}

/// Creates an identity file that only this user can read, and answers its peer id. It never replaces a file.
///
/// # Errors
///
/// [`Error::Identity`] when the file exists or cannot be written.
pub fn create_identity(path: &Path) -> Result<PeerId, Error> {
    let key = SecretKey::random(&mut OsRng);
    let pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|_| Error::Identity("could not encode the key".to_owned()))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| Error::Identity(error.to_string()))?;
    file.write_all(pem.as_bytes())
        .map_err(|error| Error::Identity(error.to_string()))?;
    method(SigningKey::from(key), BTreeSet::new()).map(|method| method.peer_id())
}

/// The responder for the allowed peers. An empty set proves nobody.
///
/// # Errors
///
/// [`Error::Identity`] when the key is unusable.
pub fn method(key: SigningKey, allowed: BTreeSet<PeerId>) -> Result<WebCryptoMethod, Error> {
    WebCryptoMethod::new(key, allowed).map_err(|error| Error::Identity(error.to_string()))
}

/// Runs the responder's side of the proof on `channel`, within `limit`, and gives a peer that proved itself `max_message`
/// from before the last message of the proof goes out, so the peer's first message after that meets the new limit. The
/// peer it proved.
///
/// # Errors
///
/// [`Error::Proof`] for a refused, malformed or too-slow proof, and [`Error::Channel`] when the channel fails.
pub fn prove<C: Channel>(
    method: &WebCryptoMethod,
    channel: &C,
    limit: Duration,
    max_message: usize,
) -> Result<PeerId, Error> {
    let deadline = Instant::now() + limit;
    let mut proof = method
        .respond(None)
        .map_err(|error| Error::Proof(error.to_string()))?;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Proof("no proof in time".to_owned()));
        }
        let message = match channel.receive(Some(left)) {
            Ok(Some(message)) => message,
            Ok(None) => return Err(Error::Proof("the peer left".to_owned())),
            Err(Failure::Timeout) => return Err(Error::Proof("no proof in time".to_owned())),
            Err(failure) => return Err(failure.into()),
        };
        if message.len() > MAX_PROOF_MESSAGE {
            return Err(Error::Proof("a proof message is over its limit".to_owned()));
        }
        proof
            .receive(&message)
            .map_err(|error| Error::Proof(error.to_string()))?;
        let mut outgoing = Vec::new();
        while let Some(out) = proof
            .transmit()
            .map_err(|error| Error::Proof(error.to_string()))?
        {
            outgoing.push(out);
        }
        if proof.is_complete() {
            channel.set_max_message(max_message);
        }
        for out in &outgoing {
            channel.send(out)?;
        }
        if proof.is_complete() {
            let established = proof
                .finish()
                .map_err(|error| Error::Proof(error.to_string()))?;
            return Ok(established.peer);
        }
    }
}
