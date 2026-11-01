//! PIN pairing over any channel, so it works through a reverse proxy that ends TLS.
//!
//! Two strangers run the WebCrypto handshake trusting anyone who proves a key, then pair inside that session: the PIN confirms
//! both keys, both PAKE messages and a secret export of the session. A relay that swaps a key or changes a message fails on both
//! sides. These connections only pair and never serve an agent: a peer connects again, pinned, and only then gets one.
//! Only PIN mode is offered, and only the PIN the responder shows.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use coqui_peer_webcrypto::pair::{
    Decision, Paired, Pairing, PairingDesk, PairingOptions, PairingResponder, Pin, PinRole,
};
use coqui_peer_webcrypto::{Identity, Session, Trust, TrustStore, WebCrypto, random};
use coqui_web::Channel;

use crate::Error;
use crate::auth::{MAX_PROOF_MESSAGE, next, prove, prove_out};

fn failed(error: impl core::fmt::Display) -> Error {
    Error::Proof(error.to_string())
}

fn send<C: Channel>(channel: &C, session: &mut Session, message: &[u8]) -> Result<(), Error> {
    let record = session.seal(message).map_err(failed)?;
    channel.send(&record).map_err(Error::from)
}

fn open<C: Channel>(
    channel: &C,
    session: &mut Session,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    session.open(&next(channel, deadline)?).map_err(failed)
}

/// A desk that takes only PIN runs, as [`respond`] needs: a joiner asking for another mode is told it is disabled.
///
/// # Errors
///
/// [`Error::Proof`] when the options are refused.
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

/// The side that shows the PIN, on a [`desk`]. A joiner it pairs with is accepted in `store` before the joiner is told.
///
/// # Errors
///
/// [`Error::Proof`] for a wrong PIN, a changed message or a failed proof; [`Error::Channel`] for silence or a failed channel.
pub fn respond<C: Channel>(
    channel: &C,
    identity: &Identity,
    store: &TrustStore,
    desk: &PairingDesk,
    source: Option<SocketAddr>,
    limit: Duration,
) -> Result<Paired, Error> {
    let deadline = Instant::now() + limit;
    let method = WebCrypto::new(identity.clone()).with_initiators(Trust::AnyWithKeyProof);
    let established = prove(&method, channel, limit, MAX_PROOF_MESSAGE)?;
    let remote = established
        .key
        .clone()
        .ok_or_else(|| Error::Proof("the peer proved no key".to_owned()))?;
    let mut session = established.session;
    let side = session.pair_channel(identity, remote).map_err(failed)?;
    let mut responder = PairingResponder::new(side, store, desk, source);
    loop {
        while let Some(message) = responder.transmit() {
            send(channel, &mut session, &message)?;
        }
        if responder.is_complete() {
            return responder.finish().map_err(failed);
        }
        let step = match responder.awaiting() {
            // Nobody is here to enter a PIN the joiner shows, so that run ends now rather than holding this one open.
            Some(_) => responder.resume(Decision::Denied),
            None => responder.receive(&open(channel, &mut session, deadline)?),
        };
        // A refusal has a message for the joiner, which goes out before the error does.
        if let Err(error) = step {
            while let Some(message) = responder.transmit() {
                let _ = send(channel, &mut session, &message);
            }
            return Err(failed(error));
        }
    }
}

/// The side that was given the PIN. The responder's key is accepted in `store` once it is confirmed.
///
/// # Errors
///
/// As [`respond`].
pub fn join<C: Channel>(
    channel: &C,
    identity: &Identity,
    pin: Pin,
    store: &TrustStore,
    limit: Duration,
) -> Result<Paired, Error> {
    join_as(
        channel,
        identity,
        Pairing::pin(pin, PinRole::Typed),
        store,
        limit,
    )
}

/// [`join`] asking for any mode, to show what a responder refuses.
#[doc(hidden)]
pub fn join_as<C: Channel>(
    channel: &C,
    identity: &Identity,
    pairing: Pairing,
    store: &TrustStore,
    limit: Duration,
) -> Result<Paired, Error> {
    let deadline = Instant::now() + limit;
    let method = WebCrypto::new(identity.clone()).with_responders(Trust::AnyWithKeyProof);
    let established = prove_out(&method, channel, limit, MAX_PROOF_MESSAGE)?;
    let remote = established
        .key
        .clone()
        .ok_or_else(|| Error::Proof("the peer proved no key".to_owned()))?;
    let mut session = established.session;
    let side = session.pair_channel(identity, remote).map_err(failed)?;
    let mut joiner = pairing.join(side).map_err(failed)?;
    loop {
        while let Some(message) = joiner.transmit() {
            send(channel, &mut session, &message)?;
        }
        if joiner.is_complete() {
            break;
        }
        let message = open(channel, &mut session, deadline)?;
        joiner.receive(&message).map_err(failed)?;
    }
    let paired = joiner.finish().map_err(failed)?;
    match store.accept_key(&paired.peer) {
        Ok(_) | Err(coqui_peer::Error::AlreadyAccepted) => Ok(paired),
        Err(error) => Err(failed(error)),
    }
}
