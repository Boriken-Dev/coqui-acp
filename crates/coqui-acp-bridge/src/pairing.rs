//! PIN pairing over any channel, so it works through a reverse proxy that ends TLS.
//!
//! The PIN authenticates: SPAKE2 confirms both keys, both PAKE messages and the binding, so a relay that only forwards pairs the
//! two real keys, and one that swaps a key or changes a message fails on both sides. Nothing depends on the connection's security.
//! Only PIN mode is offered here. An invitation sends its secret as is and the device flow needs a verified channel, and neither
//! is safe on a channel that proves nothing.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use coqui_peer::TrustStore;
use coqui_peer_tls::pair::{
    Channel as PairChannel, Decision, Paired, Pairing, PairingDesk, PairingOptions,
    PairingResponder, Pin, PinRole,
};
use coqui_peer_tls::{Identity, certificate_key, random};
use coqui_web::{Channel, Failure};
use rand_core::{OsRng, RngCore};

use crate::Error;

/// The size of the id that ties a run's messages to this one connection. Not a secret.
const BINDING: usize = 32;

fn failed(error: impl core::fmt::Display) -> Error {
    Error::Proof(error.to_string())
}

fn next<C: Channel>(channel: &C, deadline: Instant) -> Result<Vec<u8>, Error> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Error::Proof("no answer in time".to_owned()));
    }
    match channel.receive(Some(left)) {
        Ok(Some(message)) => Ok(message),
        Ok(None) => Err(Error::Proof("the peer left".to_owned())),
        Err(Failure::Timeout) => Err(Error::Proof("no answer in time".to_owned())),
        Err(failure) => Err(failure.into()),
    }
}

/// A desk that takes only PIN runs, as [`respond`] needs: a joiner asking for another mode is told it is disabled.
///
/// # Errors
///
/// [`Error::Proof`] when the operating system's generator cannot be used.
pub fn desk() -> Result<PairingDesk, Error> {
    let options = PairingOptions {
        invitation: false,
        device: false,
        ..PairingOptions::default()
    };
    PairingDesk::new(random())
        .with_options(options)
        .map_err(failed)
}

/// The side that shows the PIN, on a [`desk`]. It opens with a random id both ends bind the run to, then answers until the exchange ends.
/// A joiner it pairs with is accepted in `store` before the joiner is told.
///
/// # Errors
///
/// [`Error::Proof`] for a wrong PIN, a changed message or silence; [`Error::Channel`] when the channel fails.
pub fn respond<C: Channel>(
    channel: &C,
    identity: &Identity,
    store: &TrustStore,
    desk: &PairingDesk,
    source: Option<SocketAddr>,
    limit: Duration,
) -> Result<Paired, Error> {
    let deadline = Instant::now() + limit;
    let mut binding = vec![0_u8; BINDING];
    OsRng.fill_bytes(&mut binding);
    channel.send(&binding)?;
    let local = certificate_key(identity.certificate_der()).map_err(failed)?;
    let side = PairChannel {
        local,
        remote: None,
        verified: false,
        binding: Some(binding),
    };
    let mut responder = PairingResponder::new(side, store, desk, source);
    loop {
        while let Some(message) = responder.transmit() {
            channel.send(&message)?;
        }
        if responder.is_complete() {
            return responder.finish().map_err(failed);
        }
        let step = match responder.awaiting() {
            // Nobody is here to enter a PIN the joiner shows, so that run ends now rather than holding this one open.
            Some(_) => responder.resume(Decision::Denied),
            None => responder.receive(&next(channel, deadline)?),
        };
        // A refusal has a message for the joiner, which goes out before the error does.
        if let Err(error) = step {
            while let Some(message) = responder.transmit() {
                let _ = channel.send(&message);
            }
            return Err(failed(error));
        }
    }
}

/// The side that was given the PIN. The responder's key is accepted in `store` once it is confirmed.
///
/// # Errors
///
/// [`Error::Proof`] for a wrong PIN, a changed message or silence; [`Error::Channel`] when the channel fails.
pub fn join<C: Channel>(
    channel: &C,
    identity: &Identity,
    pin: Pin,
    store: &TrustStore,
    limit: Duration,
) -> Result<Paired, Error> {
    let deadline = Instant::now() + limit;
    let binding = next(channel, deadline)?;
    if binding.len() != BINDING {
        return Err(Error::Proof("the peer did not open a pairing".to_owned()));
    }
    let local = certificate_key(identity.certificate_der()).map_err(failed)?;
    let side = PairChannel {
        local,
        remote: None,
        verified: false,
        binding: Some(binding),
    };
    let mut joiner = Pairing::pin(pin, PinRole::Typed)
        .join(side)
        .map_err(failed)?;
    loop {
        while let Some(message) = joiner.transmit() {
            channel.send(&message)?;
        }
        if joiner.is_complete() {
            break;
        }
        joiner.receive(&next(channel, deadline)?).map_err(failed)?;
    }
    let paired = joiner.finish().map_err(failed)?;
    match store.accept_key(&paired.peer) {
        Ok(_) | Err(coqui_peer::Error::AlreadyAccepted) => Ok(paired),
        Err(error) => Err(failed(error)),
    }
}
