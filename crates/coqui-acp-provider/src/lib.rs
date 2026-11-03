//! A Guatiao provider kind for agents.
#![forbid(unsafe_code)]

extern crate alloc;

mod kind;

pub use guatiao;
pub use kind::{
    AgentEvent, AgentMessage, AgentProvider, AgentProviderVtable, AgentSession, AgentSessionVtable,
    BLOCKED_EVENT, CANCELLED_EVENT, COMPLETED_EVENT, DENIED_EVENT, FAILED_EVENT, LIMIT_EVENT,
    QuotaReport, ResumeRequest, StartRequest, TEXT_EVENT, TOOL_EVENT, TURN_CANCELLED, TokenUsage,
    UsageLimit,
};
