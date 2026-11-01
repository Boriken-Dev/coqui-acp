//! Passes complete messages between a channel and an agent, unread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;

use coqui_web::{Channel, Failure};

use crate::agent::{Agent, Output};

/// How often each direction looks up to see whether the other has ended.
const POLL: Duration = Duration::from_millis(50);

/// Why one link ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LinkEnd {
    /// The agent's output ended, which means it exited or closed it.
    AgentEnded,
    /// The peer closed in order.
    PeerClosed,
    /// The channel failed. A dialing bridge may connect again.
    Failed(Failure),
    /// A message would have corrupted the line protocol or was over its limit.
    Protocol,
}

/// Relays until the agent or the link ends. A line the peer could not take stays lost: ACP's own session
/// recovery is the only safe answer to that, so nothing here replays anything.
pub fn relay<C: Channel>(channel: &C, agent: &Agent) -> LinkEnd {
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let inbound = scope.spawn(|| {
            let ended = from_peer(channel, agent, &stop);
            stop.store(true, Ordering::SeqCst);
            ended
        });
        let outbound = to_peer(channel, agent, &stop);
        stop.store(true, Ordering::SeqCst);
        let inbound = inbound.join().unwrap_or(Some(LinkEnd::Protocol));
        outbound.or(inbound).unwrap_or(LinkEnd::PeerClosed)
    })
}

fn from_peer<C: Channel>(channel: &C, agent: &Agent, stop: &AtomicBool) -> Option<LinkEnd> {
    while !stop.load(Ordering::SeqCst) {
        match channel.receive(Some(POLL)) {
            Ok(Some(message)) => {
                // One message is one line to the agent: a raw newline inside would forge a second one.
                if message.iter().any(|byte| matches!(byte, b'\n' | b'\r')) {
                    return Some(LinkEnd::Protocol);
                }
                if agent.write_line(&message).is_err() {
                    return Some(LinkEnd::AgentEnded);
                }
            }
            Ok(None) => return Some(LinkEnd::PeerClosed),
            Err(Failure::Timeout) => {}
            Err(failure) => return Some(LinkEnd::Failed(failure)),
        }
    }
    None
}

fn to_peer<C: Channel>(channel: &C, agent: &Agent, stop: &AtomicBool) -> Option<LinkEnd> {
    let output = agent.output();
    while !stop.load(Ordering::SeqCst) {
        match output.recv_timeout(POLL) {
            Ok(Output::Line(line)) => {
                if let Err(failure) = channel.send(&line) {
                    return Some(if failure == Failure::Protocol {
                        LinkEnd::Protocol
                    } else {
                        LinkEnd::Failed(failure)
                    });
                }
            }
            Ok(Output::TooLong) => return Some(LinkEnd::Protocol),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Some(LinkEnd::AgentEnded),
        }
    }
    None
}
