use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::fmt;

use coqui_acp::secret::{
    Context, Form, Method, RequestParams, RequestReply, ShowDeliver, ShowDone, ShowOpen, ShowOpened,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::files::shown;
use crate::host::FOLDER_NOTICE;
use crate::run::{CommandLine, CommandSpec};
use crate::session::{Core, Inner};
use crate::{Action, Approval, Error, Event, program};

/// One tool the agent is offered, in the shape MCP's `tools/list` gives.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ToolDefinition {
    /// The tool's name.
    pub name: &'static str,
    /// What the agent is told it does.
    pub description: &'static str,
    /// The JSON Schema of its arguments.
    pub input_schema: Value,
}

/// What a tool call gives back to the agent: JSON text, scrubbed of every secret the session holds.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolResult {
    /// The result as JSON text.
    pub text: String,
    /// Whether the call failed; `text` is then `{"error": ...}`.
    pub is_error: bool,
}

impl fmt::Debug for ToolResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolResult")
            .field("text", &self.text)
            .field("is_error", &self.is_error)
            .finish()
    }
}

/// The tools of one session. Cloning is cheap and every clone is the same session.
#[derive(Clone)]
pub struct SessionTools {
    inner: Arc<Inner>,
    core: Arc<Core>,
}

impl fmt::Debug for SessionTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTools")
            .field("session", &self.core.id)
            .field("secrets", &self.core.handle_count())
            .finish_non_exhaustive()
    }
}

const REQUEST: &str = "secret_request";
const USE: &str = "secret_use";
const SHOW: &str = "secret_show";

fn form_schema_property(what: &str) -> Value {
    json!({
        "type": "object",
        "description": what,
    })
}

impl SessionTools {
    pub(crate) fn new(inner: Arc<Inner>, core: Arc<Core>) -> Self {
        Self { inner, core }
    }

    /// The session's id.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.core.id
    }

    /// How many secrets the session holds now.
    #[must_use]
    pub fn handle_count(&self) -> usize {
        self.core.handle_count()
    }

    /// The tools this session serves: only those the frontend's capability offers (`secret_use` needs a way to get a secret, so it
    /// needs `request`).
    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let capability = &self.core.capability;
        let mut tools = Vec::new();
        if capability.request {
            tools.push(ToolDefinition {
                name: REQUEST,
                description: "Ask the person for values, secrets included, in a form on their own screen. The answer does not come to you: \
                    you get the plain values they typed and, for each secret field (`writeOnly: true`), only a handle. Use the handle \
                    with secret_use. Mark every secret field `writeOnly: true` and give it no default.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": {"type": "string", "description": "What the person is asked and why."},
                        "schema": form_schema_property("A flat JSON Schema object: `properties` are the questions; a secret one is `writeOnly: true`."),
                        "form": form_schema_property("Optional layout: sections and, per question, `widget`, `placeholder`, `unit`, `visibleWhen`.")
                    },
                    "required": ["message", "schema"],
                    "additionalProperties": false
                }),
            });
            tools.push(ToolDefinition {
                name: USE,
                description: "Use a secret by its handle without seeing it: write it to a file under the working directories, or run a \
                    command with it in an environment variable. The person is asked to approve every use, showing the file or the \
                    command in full. Give exactly one of `file` or `env`. Output that holds the secret has it replaced by the handle.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "handle": {"type": "string"},
                        "file": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string", "description": "Under the working directories; `..` is refused."},
                                "mode": {"type": "string", "description": "Octal, such as \"0600\" (the default); Unix only."},
                                "overwrite": {"type": "boolean", "description": "Replace an existing file. Default false."}
                            },
                            "required": ["path"],
                            "additionalProperties": false
                        },
                        "env": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "description": "The environment variable that holds the secret."},
                                "command": {"description": "An array of strings (a program and its arguments) or a string for the shell, which can expand the variable.",
                                    "oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
                                "cwd": {"type": "string", "description": "Under the working directories; the first one by default."}
                            },
                            "required": ["name", "command"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["handle"],
                    "additionalProperties": false
                }),
            });
        }
        if capability.show {
            tools.push(ToolDefinition {
                name: SHOW,
                description: "Show the person a value on their own screen without it passing through you. `schema` and `form` are as \
                    for secret_request: the schema's keywords say how the frontend displays each field. `values` are plain values to \
                    show beside them. `secrets` gives, for each secret field, where the value comes from: a `handle`, a `file` under \
                    the working directories, an `env` variable of the host, or a `command` whose output is the value. An `env` or \
                    `command` source needs the person's approval.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": {"type": "string"},
                        "schema": form_schema_property("A flat JSON Schema object; a secret field is `writeOnly: true`."),
                        "form": form_schema_property("Optional layout."),
                        "values": form_schema_property("Plain values to show."),
                        "secrets": {
                            "type": "object",
                            "description": "Field name to source: one of {\"handle\"}, {\"file\"}, {\"env\"}, {\"command\", \"cwd\"?}.",
                            "additionalProperties": {"type": "object"}
                        }
                    },
                    "required": ["message", "schema", "secrets"],
                    "additionalProperties": false
                }),
            });
        }
        tools
    }

    /// Calls a tool. The result is scrubbed of every secret of the session (the value and its common encodings) whatever the tool
    /// made, and a failure is a result with `is_error` set and a reason that holds no value.
    #[must_use]
    pub fn call(&self, name: &str, arguments: &Value) -> ToolResult {
        let outcome = match name {
            REQUEST if self.core.capability.request => self.request(arguments),
            USE if self.core.capability.request => self.use_secret(arguments),
            SHOW if self.core.capability.show => self.show(arguments),
            _ => Err(Error::Arguments("no such tool")),
        };
        let (mut value, is_error) = match outcome {
            Ok(value) => (value, false),
            Err(error) => (json!({"error": error.to_string()}), true),
        };
        self.core
            .scrubber(&self.inner.settings)
            .scrub_value(&mut value);
        ToolResult {
            text: value.to_string(),
            is_error,
        }
    }

    fn require(&self, offered: bool, what: &'static str) -> Result<(), Error> {
        if offered {
            Ok(())
        } else {
            Err(Error::NotOffered(what))
        }
    }

    fn request(&self, args: &Value) -> Result<Value, Error> {
        let settings = &self.inner.settings;
        self.require(self.core.capability.request, "secret requests")?;
        let ask = Ask::read(args, settings, false)?;
        let form = Form::parse_text(&ask.schema, &settings.limits)?;
        let secrets = form.fields.iter().filter(|field| field.secret).count();
        if secrets == 0 {
            return Err(Error::Arguments(
                "the schema has no secret field: mark one writeOnly, or ask for plain values another way",
            ));
        }
        self.core.check_room(secrets, settings, self.inner.now())?;
        let _busy = self.core.busy()?;
        let request_id = Uuid::new_v4().to_string();
        let context = Context {
            session_id: self.core.id.clone(),
            request_id: request_id.clone(),
            message: ask.message,
            schema: ask.schema,
            form: ask.form,
            values: None,
        };
        let now = self.inner.now();
        self.admit(&request_id, now)?;
        let receiver = coqui_acp::secret::Receiver::new(
            Method::Request,
            &context,
            &settings.limits,
            &self.inner.prover,
            &self.core.owner,
            now,
            settings.answer_lifetime,
        )?;
        let params = RequestParams {
            context,
            recipient: receiver.recipient().clone(),
        };
        // The only frontend ever asked is the session owner's.
        let reply = self.core.frontend.request(&params)?;
        self.complete(&request_id)?;
        let RequestReply::Accept {
            plain,
            sealed,
            auth,
        } = reply
        else {
            let status = if matches!(reply, RequestReply::Decline) {
                "declined"
            } else {
                "cancelled"
            };
            return Ok(json!({"status": status}));
        };
        let now = self.inner.now();
        let opened = receiver.open_values(
            &self.inner.verifier,
            &self.core.owner,
            &plain,
            &sealed,
            &auth,
            now,
        )?;
        let made = self.core.store(opened.secrets, settings, now)?;
        let mut handles = Map::new();
        for (field, handle) in made {
            self.inner.emit(&Event::Stored {
                session_id: self.core.id.clone(),
                handle: handle.clone(),
                secret: field.clone(),
            });
            handles.insert(field, Value::String(handle));
        }
        Ok(json!({"status": "accepted", "plain": opened.plain, "handles": handles}))
    }

    fn admit(&self, id: &str, now: u64) -> Result<(), Error> {
        self.inner
            .ledger
            .lock()
            .map_err(|_| Error::Replayed)?
            .admit(id, now, self.inner.settings.request_memory.as_secs())
    }

    fn complete(&self, id: &str) -> Result<(), Error> {
        self.inner
            .ledger
            .lock()
            .map_err(|_| Error::Replayed)?
            .complete(id)
    }

    fn use_secret(&self, args: &Value) -> Result<Value, Error> {
        let settings = &self.inner.settings;
        self.require(self.core.capability.request, "secret requests")?;
        let map = args
            .as_object()
            .ok_or(Error::Arguments("the arguments are not an object"))?;
        let handle = string(map, "handle")?;
        // One use at a time: the person is asked about one thing, and a command's output is scrubbed against one state.
        let _busy = self.core.busy()?;
        let field = self.core.field_of(&handle, settings, self.inner.now())?;
        match (map.get("file"), map.get("env")) {
            (Some(file), None) => self.write_file(&handle, &field, file),
            (None, Some(env)) => self.run_with(&handle, &field, env),
            _ => Err(Error::Arguments("give exactly one of file or env")),
        }
    }

    fn write_file(&self, handle: &str, field: &str, file: &Value) -> Result<Value, Error> {
        let settings = &self.inner.settings;
        let map = file
            .as_object()
            .ok_or(Error::Arguments("file is not an object"))?;
        let path = string(map, "path")?;
        let mode = match map.get("mode") {
            None => settings.file_mode,
            Some(Value::String(text)) => {
                u32::from_str_radix(text, 8).map_err(|_| Error::Arguments("mode is not octal"))?
            }
            Some(_) => return Err(Error::Arguments("mode is not text")),
        };
        if mode & !settings.max_file_mode != 0 {
            return Err(Error::Limit("the mode is wider than the settings allow"));
        }
        let overwrite = match map.get("overwrite") {
            None => false,
            Some(Value::Bool(overwrite)) => *overwrite,
            Some(_) => return Err(Error::Arguments("overwrite is not a boolean")),
        };
        let resolved = self.core.places.resolve(&path, 1)?;
        self.inner.approve(
            &self.core,
            &Approval {
                session_id: self.core.id.clone(),
                handle: Some(handle.to_owned()),
                secret: field.to_owned(),
                notice: None,
                action: Action::WriteFile {
                    path: resolved.path.clone(),
                    mode,
                    mode_applied: cfg!(unix),
                    overwrite,
                    under_roots: resolved.in_roots,
                },
            },
        )?;
        // The person approved this place: if a link was swapped in while they decided, it is not this place any more.
        if self.core.places.resolve(&path, 1)? != resolved {
            return Err(Error::Path("the place changed after it was approved"));
        }
        let value = self.core.value_of(handle, settings, self.inner.now())?;
        self.core.check_alive()?;
        self.core.places.write(&resolved, &value, mode, overwrite)?;
        self.inner.emit(&Event::Wrote {
            session_id: self.core.id.clone(),
            path: resolved.path.clone(),
        });
        Ok(json!({
            "status": "written",
            "path": shown(&resolved.path),
            "under_roots": resolved.in_roots,
        }))
    }

    fn run_with(&self, handle: &str, field: &str, env: &Value) -> Result<Value, Error> {
        let settings = &self.inner.settings;
        let map = env
            .as_object()
            .ok_or(Error::Arguments("env is not an object"))?;
        let variable = string(map, "name")?;
        check_variable(&variable, &settings.denied_variables)?;
        let command = CommandLine::from_value(
            map.get("command")
                .ok_or(Error::Arguments("env has no command"))?,
            settings.max_command_bytes,
        )?;
        let cwd = self
            .core
            .places
            .directory(map.get("cwd").map(as_text).transpose()?)?;
        let launch = program::plan(&command, &cwd, &self.core.places)?;
        self.inner.approve(
            &self.core,
            &Approval {
                session_id: self.core.id.clone(),
                handle: Some(handle.to_owned()),
                secret: field.to_owned(),
                notice: Some(FOLDER_NOTICE),
                action: Action::RunCommand {
                    variable: variable.clone(),
                    command: command.display(),
                    program: launch.program.clone(),
                    cwd: cwd.clone(),
                },
            },
        )?;
        let value = self.core.value_of(handle, settings, self.inner.now())?;
        let scrubber = self.core.scrubber(settings);
        let spec = CommandSpec {
            command: &command,
            program: &launch.program,
            path: Some(&launch.path),
            cwd: &cwd,
            secret: Some((&variable, &value)),
            timeout: settings.command_timeout,
            max_output: settings.max_output_bytes,
            inherit_env: &settings.inherit_env,
        };
        self.inner.emit(&Event::Ran {
            session_id: self.core.id.clone(),
            command: command.display(),
        });
        self.core.check_alive()?;
        let output = self.inner.runner.run(&spec)?;
        // The runner cut the raw output at the limit, perhaps inside the secret: what could be the start of one is dropped before
        // scrubbing, which cannot find a piece of a secret. The cut is made on the raw bytes, never on what scrubbing leaves.
        let raw = |bytes: &[u8]| -> Vec<u8> {
            let keep = if output.truncated {
                bytes
                    .len()
                    .saturating_sub(scrubber.longest().saturating_sub(1))
            } else {
                bytes.len()
            };
            bytes[..keep].to_vec()
        };
        let (stdout, cut_out) = clip(
            scrubber.scrub_output(&raw(&output.stdout)),
            settings.max_output_bytes,
        );
        let (stderr, cut_err) = clip(
            scrubber.scrub_output(&raw(&output.stderr)),
            settings.max_output_bytes,
        );
        Ok(json!({
            "status": if output.timed_out { "timed_out" } else { "exited" },
            "exit_code": output.status,
            "stdout": stdout,
            "stderr": stderr,
            "truncated": output.truncated || cut_out || cut_err,
        }))
    }

    fn show(&self, args: &Value) -> Result<Value, Error> {
        let settings = &self.inner.settings;
        self.require(self.core.capability.show, "secret shows")?;
        let ask = Ask::read(args, settings, true)?;
        let form = Form::parse_text(&ask.schema, &settings.limits)?;
        let wanted: Vec<&str> = form
            .fields
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.field.name.as_str())
            .collect();
        if wanted.is_empty() {
            return Err(Error::Arguments("the schema has no secret field to show"));
        }
        let sources = Source::read_all(args, &wanted, settings.max_command_bytes)?;
        let _busy = self.core.busy()?;
        // Everything that needs the person's yes is asked before anything runs.
        let mut places = BTreeMap::new();
        for (name, source) in &sources {
            places.insert(name.as_str(), self.approve_source(name, source)?);
        }
        self.core.check_alive()?;
        let mut values: BTreeMap<String, Zeroizing<String>> = BTreeMap::new();
        for (name, source) in &sources {
            let value = self.fetch(source, places[name.as_str()].as_ref())?;
            values.insert(name.clone(), value);
        }
        let request_id = Uuid::new_v4().to_string();
        let now = self.inner.now();
        self.admit(&request_id, now)?;
        let context = Context {
            session_id: self.core.id.clone(),
            request_id: request_id.clone(),
            message: ask.message,
            schema: ask.schema,
            form: ask.form,
            values: Some(ask.values),
        };
        let plain = context.values.clone().unwrap_or_default();
        let opened = self.core.frontend.show_open(&ShowOpen {
            context: context.clone(),
        })?;
        let ShowOpened::Ready(recipient) = opened else {
            self.complete(&request_id)?;
            let status = if matches!(opened, ShowOpened::Decline) {
                "declined"
            } else {
                "cancelled"
            };
            return Ok(json!({"status": status}));
        };
        let now = self.inner.now();
        let collector = self.inner.verifier.collector(
            &self.core.owner,
            Method::Show,
            &context,
            &recipient,
            now,
        )?;
        let mut sealed = BTreeMap::new();
        for (name, value) in &values {
            sealed.insert(name.clone(), collector.seal(name, value)?);
        }
        let auth = collector.authenticate(
            &self.inner.prover,
            &plain,
            &sealed,
            now,
            settings.answer_lifetime,
        )?;
        let done = self.core.frontend.show_deliver(&ShowDeliver {
            session_id: self.core.id.clone(),
            request_id: request_id.clone(),
            sealed,
            auth,
        })?;
        self.complete(&request_id)?;
        Ok(match done {
            ShowDone::Shown => {
                self.inner.emit(&Event::Shown {
                    session_id: self.core.id.clone(),
                });
                json!({"status": "shown"})
            }
            _ => json!({"status": "cancelled"}),
        })
    }

    /// Asks for what a source needs, and returns the directory a command source starts in.
    fn approve_source(
        &self,
        field: &str,
        source: &Source,
    ) -> Result<Option<(std::path::PathBuf, program::Launch)>, Error> {
        let (action, handle, cwd, notice) = match source {
            Source::Handle(handle) => {
                self.core
                    .field_of(handle, &self.inner.settings, self.inner.now())?;
                return Ok(None);
            }
            Source::File(path) => {
                // Read now, so a path outside the roots is refused before anyone is asked about anything else.
                self.core.places.resolve(path, 0).and_then(|resolved| {
                    if resolved.in_roots {
                        Ok(())
                    } else {
                        Err(Error::Path("not under the session's roots"))
                    }
                })?;
                return Ok(None);
            }
            Source::Env(name) => {
                check_variable_name(name)?;
                (Action::ShowEnv { name: name.clone() }, None, None, None)
            }
            Source::Command { line, cwd } => {
                let cwd = self.core.places.directory(cwd.as_deref())?;
                let launch = program::plan(line, &cwd, &self.core.places)?;
                (
                    Action::ShowCommand {
                        command: line.display(),
                        program: launch.program.clone(),
                        cwd: cwd.clone(),
                    },
                    None,
                    Some((cwd, launch)),
                    Some(FOLDER_NOTICE),
                )
            }
        };
        self.inner.approve(
            &self.core,
            &Approval {
                session_id: self.core.id.clone(),
                handle,
                secret: field.to_owned(),
                notice,
                action,
            },
        )?;
        Ok(cwd)
    }

    /// The text a source yields, held where it is zeroed when dropped.
    fn fetch(
        &self,
        source: &Source,
        cwd: Option<&(std::path::PathBuf, program::Launch)>,
    ) -> Result<Zeroizing<String>, Error> {
        let settings = &self.inner.settings;
        let bytes: Zeroizing<Vec<u8>> = match source {
            Source::Handle(handle) => self.core.value_of(handle, settings, self.inner.now())?,
            Source::File(path) => self
                .core
                .places
                .read(path, settings.limits.value_bytes + 2)?,
            Source::Env(name) => Zeroizing::new(
                std::env::var(name)
                    .map_err(|_| Error::Command("the variable is not set"))?
                    .into_bytes(),
            ),
            Source::Command { line, .. } => {
                let (cwd, launch) = cwd.ok_or(Error::Command("no directory"))?;
                self.inner.emit(&Event::Ran {
                    session_id: self.core.id.clone(),
                    command: line.display(),
                });
                let output = self.inner.runner.run(&CommandSpec {
                    command: line,
                    program: &launch.program,
                    path: Some(&launch.path),
                    cwd,
                    secret: None,
                    timeout: settings.command_timeout,
                    max_output: settings.limits.value_bytes + 2,
                    inherit_env: &settings.inherit_env,
                })?;
                if output.timed_out || output.status != Some(0) || output.truncated {
                    // What it printed may be the secret or part of it: none of it is said.
                    return Err(Error::Command(
                        "failed, timed out or printed more than a value may be",
                    ));
                }
                output.stdout
            }
        };
        let mut text = Zeroizing::new(
            core::str::from_utf8(&bytes)
                .map_err(|_| Error::Command("the value is not text"))?
                .to_owned(),
        );
        // A line break the source added is not part of the value.
        if text.ends_with('\n') {
            text.pop();
            if text.ends_with('\r') {
                text.pop();
            }
        }
        Ok(text)
    }
}

/// `text` cut to at most `max` bytes at a character boundary, and whether it was cut.
fn clip(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut at = max;
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    text.truncate(at);
    (text, true)
}

/// A request's or a show's message, schema and layout as the text that is sent.
struct Ask {
    message: String,
    schema: String,
    form: Option<String>,
    values: String,
}

impl Ask {
    fn read(args: &Value, settings: &crate::Settings, show: bool) -> Result<Self, Error> {
        let max_message = settings.max_message_bytes;
        let map = args
            .as_object()
            .ok_or(Error::Arguments("the arguments are not an object"))?;
        let message = string(map, "message")?;
        if message.len() > max_message {
            return Err(Error::Limit(
                "the message is longer than the settings allow",
            ));
        }
        let values = if show {
            match map.get("values") {
                None => "{}".to_owned(),
                Some(Value::Object(values)) => {
                    let text = Value::Object(values.clone()).to_string();
                    if text.len() > settings.max_values_bytes {
                        return Err(Error::Limit(
                            "the plain values are longer than the settings allow",
                        ));
                    }
                    text
                }
                Some(_) => return Err(Error::Arguments("values is not an object")),
            }
        } else {
            String::new()
        };
        Ok(Self {
            message,
            schema: json_text(map.get("schema").ok_or(Error::Arguments("no schema"))?)?,
            form: map.get("form").map(json_text).transpose()?,
            values,
        })
    }
}

/// An object as the JSON text that is sent and hashed, or a string that is that text already.
fn json_text(value: &Value) -> Result<String, Error> {
    match value {
        Value::Object(_) => Ok(value.to_string()),
        Value::String(text) => Ok(text.clone()),
        _ => Err(Error::Arguments("schema and form are JSON objects")),
    }
}

fn as_text(value: &Value) -> Result<&str, Error> {
    value.as_str().ok_or(Error::Arguments("not text"))
}

fn string(map: &Map<String, Value>, key: &'static str) -> Result<String, Error> {
    match map.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Ok(text.clone()),
        _ => Err(Error::Arguments("a required text argument is missing")),
    }
}

/// Where a shown secret comes from.
enum Source {
    Handle(String),
    File(String),
    Env(String),
    Command {
        line: CommandLine,
        cwd: Option<String>,
    },
}

impl Source {
    /// The `secrets` argument: exactly the secret fields of the form, each with one source.
    fn read_all(
        args: &Value,
        wanted: &[&str],
        max_command: usize,
    ) -> Result<BTreeMap<String, Self>, Error> {
        let given = args
            .get("secrets")
            .and_then(Value::as_object)
            .ok_or(Error::Arguments("secrets is not an object"))?;
        if given.len() != wanted.len() || wanted.iter().any(|name| !given.contains_key(*name)) {
            return Err(Error::Arguments(
                "secrets must name each secret field of the schema once",
            ));
        }
        given
            .iter()
            .map(|(name, source)| Ok((name.clone(), Self::read(source, max_command)?)))
            .collect()
    }

    fn read(value: &Value, max_command: usize) -> Result<Self, Error> {
        let map = value
            .as_object()
            .ok_or(Error::Arguments("a source is not an object"))?;
        let kinds = ["handle", "file", "env", "command"];
        let present: Vec<&str> = kinds
            .into_iter()
            .filter(|kind| map.contains_key(*kind))
            .collect();
        let allowed_extra = |extra: &[&str]| {
            map.keys()
                .all(|key| present.contains(&key.as_str()) || extra.contains(&key.as_str()))
        };
        match present.as_slice() {
            ["handle"] if allowed_extra(&[]) => Ok(Self::Handle(string(map, "handle")?)),
            ["file"] if allowed_extra(&[]) => Ok(Self::File(string(map, "file")?)),
            ["env"] if allowed_extra(&[]) => Ok(Self::Env(string(map, "env")?)),
            ["command"] if allowed_extra(&["cwd"]) => Ok(Self::Command {
                line: CommandLine::from_value(&map["command"], max_command)?,
                cwd: map
                    .get("cwd")
                    .map(|cwd| as_text(cwd).map(str::to_owned))
                    .transpose()?,
            }),
            _ => Err(Error::Arguments(
                "a source is exactly one of handle, file, env, command",
            )),
        }
    }
}

/// Names a secret may not be put in: ones that change how a program is found or started rather than what it reads.
fn check_variable(name: &str, denied: &[String]) -> Result<(), Error> {
    check_variable_name(name)?;
    let upper = name.to_ascii_uppercase();
    // A pattern is a name, or a prefix with a trailing `*`; compared without regard to case.
    let hit = denied.iter().any(|pattern| {
        let pattern = pattern.to_ascii_uppercase();
        pattern
            .strip_suffix('*')
            .map_or(upper == pattern, |prefix| upper.starts_with(prefix))
    });
    if hit {
        return Err(Error::Arguments(
            "that variable is on the settings' denied list: it changes how programs start and cannot carry a secret",
        ));
    }
    Ok(())
}

fn check_variable_name(name: &str) -> Result<(), Error> {
    let mut chars = name.chars();
    let ok = name.len() <= 128
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::Arguments(
            "a variable name is letters, digits and underscores, not starting with a digit",
        ))
    }
}
