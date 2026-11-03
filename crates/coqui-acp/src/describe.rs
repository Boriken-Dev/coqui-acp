//! What an agent offers before any session: the options a session starts with, its modes, and the sessions it has.
//!
//! An agent's answer is untrusted, so it is read as ACP's own types and bounded: what cannot be read is skipped,
//! lists are cut at their limit, an item whose identifier is longer than [`MAX_TEXT`] is dropped, since it could not
//! be named back, and other text is cut there. Nothing in it is acted on.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use agent_client_protocol::schema::v1::{
    ListSessionsResponse, SessionConfigKind, SessionConfigOption, SessionConfigSelectOptions,
    SessionInfo, SessionModeState,
};
use serde_json::Value;

/// The `agentCapabilities._meta` key with which an agent says it answers [`SESSION_OPTIONS_METHOD`].
pub const SESSION_OPTIONS_CAPABILITY: &str = "dev.boriken.coqui.acp/session_options";

/// The request for what a session can be started with, before any session: `{}`, answered
/// `{"configOptions": [SessionConfigOption], "modes": SessionModeState, "unknown": {part: reason}}`, where a part the
/// agent cannot tell is left out and may say why under `unknown`.
pub const SESSION_OPTIONS_METHOD: &str = "_coqui_acp/session_options";

/// The most options, modes and sessions kept.
const MAX_OPTIONS: usize = 64;
const MAX_MODES: usize = 64;
const MAX_SESSIONS: usize = 500;
/// The most values one option keeps, across its groups.
const MAX_VALUES: usize = 1024;
/// The longest text kept, in UTF-8 bytes.
const MAX_TEXT: usize = 4096;

/// One part of what an agent offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer<T> {
    /// What the agent said.
    Known(T),
    /// The agent offers the question and could not tell, for this reason, in its words.
    Unknown(String),
    /// The agent does not offer the question, so it was not asked.
    NotSupported,
}

/// What an agent offers before any session; see [`Exchange::describe`](crate::Exchange::describe).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Description {
    /// The options a session can start with, as ACP's `SessionConfigOption`s at the values a session starts with.
    /// A model is the option of category `model`, an effort the one of category `thought_level`.
    pub options: Answer<Vec<SessionConfigOption>>,
    /// The modes a session can start in.
    pub modes: Answer<SessionModeState>,
    /// The sessions the agent lists: `session/list`'s first page.
    pub sessions: Answer<Vec<SessionInfo>>,
}

/// `text` cut to [`MAX_TEXT`] bytes, on a character's boundary.
pub(crate) fn cut(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.to_string();
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn cut_in_place(text: &mut String) {
    if text.len() > MAX_TEXT {
        *text = cut(text);
    }
}

fn cut_optional(text: &mut Option<String>) {
    if let Some(text) = text {
        cut_in_place(text);
    }
}

fn fits(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_TEXT
}

/// One option, bounded, or `None` when it cannot be named.
fn option(mut option: SessionConfigOption) -> Option<SessionConfigOption> {
    if !fits(&option.id.0) {
        return None;
    }
    cut_in_place(&mut option.name);
    cut_optional(&mut option.description);
    option.meta = None;
    // A category is an identifier too, and a client does without one.
    if serde_json::to_value(&option.category)
        .ok()
        .and_then(|category| category.as_str().map(str::len))
        .is_some_and(|length| length > MAX_TEXT)
    {
        option.category = None;
    }
    if let SessionConfigKind::Select(select) = &mut option.kind {
        if !fits(&select.current_value.0) {
            return None;
        }
        let mut left = MAX_VALUES;
        match &mut select.options {
            SessionConfigSelectOptions::Ungrouped(values) => {
                values.retain(|value| fits(&value.value.0));
                values.truncate(left);
                for value in values {
                    cut_in_place(&mut value.name);
                    cut_optional(&mut value.description);
                    value.meta = None;
                }
            }
            SessionConfigSelectOptions::Grouped(groups) => {
                groups.retain(|group| fits(&group.group.0));
                for group in groups.iter_mut() {
                    cut_in_place(&mut group.name);
                    group.meta = None;
                    group.options.retain(|value| fits(&value.value.0));
                    group.options.truncate(left);
                    left -= group.options.len();
                    for value in &mut group.options {
                        cut_in_place(&mut value.name);
                        cut_optional(&mut value.description);
                        value.meta = None;
                    }
                }
            }
            _ => return None,
        }
    }
    Some(option)
}

/// What an answer says of `part`, when it does not hold the part itself.
fn unknown(answer: &Value, part: &str) -> Answer<Vec<SessionConfigOption>> {
    Answer::Unknown(why(answer, part))
}

fn why(answer: &Value, part: &str) -> String {
    match answer.get("unknown").and_then(|unknown| unknown.get(part)) {
        Some(Value::String(reason)) => cut(reason),
        _ => format!("the agent did not say what {part} it has"),
    }
}

/// The options part of a [`SESSION_OPTIONS_METHOD`] answer.
pub(crate) fn options(answer: &Value) -> Answer<Vec<SessionConfigOption>> {
    let Some(listed) = answer.get("configOptions").and_then(Value::as_array) else {
        return unknown(answer, "configOptions");
    };
    Answer::Known(
        listed
            .iter()
            .filter_map(|item| serde_json::from_value::<SessionConfigOption>(item.clone()).ok())
            .filter_map(option)
            .take(MAX_OPTIONS)
            .collect(),
    )
}

/// The modes part of a [`SESSION_OPTIONS_METHOD`] answer.
pub(crate) fn modes(answer: &Value) -> Answer<SessionModeState> {
    let Some(modes) = answer.get("modes").filter(|modes| modes.is_object()) else {
        return Answer::Unknown(why(answer, "modes"));
    };
    let Ok(mut modes) = serde_json::from_value::<SessionModeState>(modes.clone()) else {
        return Answer::Unknown("the agent's modes could not be read".to_string());
    };
    if !fits(&modes.current_mode_id.0) {
        return Answer::Unknown("the agent's modes could not be read".to_string());
    }
    modes.meta = None;
    modes.available_modes.retain(|mode| fits(&mode.id.0));
    modes.available_modes.truncate(MAX_MODES);
    for mode in &mut modes.available_modes {
        cut_in_place(&mut mode.name);
        cut_optional(&mut mode.description);
        mode.meta = None;
    }
    Answer::Known(modes)
}

/// A `session/list` answer.
pub(crate) fn sessions(answer: Value) -> Answer<Vec<SessionInfo>> {
    let Ok(listed) = serde_json::from_value::<ListSessionsResponse>(answer) else {
        return Answer::Unknown("the agent's sessions could not be read".to_string());
    };
    Answer::Known(
        listed
            .sessions
            .into_iter()
            .filter(|session| fits(&session.session_id.0) && fits(&session.cwd.to_string_lossy()))
            .take(MAX_SESSIONS)
            .map(|mut session| {
                session.additional_directories.clear();
                cut_optional(&mut session.title);
                cut_optional(&mut session.updated_at);
                session.meta = None;
                session
            })
            .collect(),
    )
}
