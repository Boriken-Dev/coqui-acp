"""An ACP agent that answers each prompt with its text, as an `acp_harness` library written in Python.

The module is the library: its id is its name, its version `__version__`, its providers the classes in
`__all__`. It is the contract agent of `coqui_acp_api::contract`, speaking ACP itself, one
JSON-RPC message per channel message, and keeps every rule of the channel: `send` waits for room in a
queue of a few messages and `close` wakes it; one thread of its own answers, in order, and is the only
caller of the sink; a sink that refuses ends the connection. It starts no program, so it lets go of the
spawner unused.
"""

from __future__ import annotations

import json
import os
import re
import threading
import uuid
from collections import deque
from typing import Any, Deque, Dict, List, Optional, Tuple

from coqui_acp_api_kinds import AcpHarness, Channel, ChannelSink, ProcessSpawner

from guatiao import Ref, provide
from guatiao.errors import BadValue, Gone, NotFound, Unsupported

__version__ = "1.0.0"
__all__ = ["EchoHarness"]

#: How many messages wait for the sink before `send` waits too.
QUEUE = 4

#: The largest message the echo takes, in bytes.
MAX_MESSAGE_BYTES = 1 << 20

#: The requirements an echo holds to: it writes nothing and runs nothing.
HOLDS = ("read_only", "no_commands")

#: The requirement names with a fixed meaning, by what their value is.
NO_VALUE = ("read_only", "no_commands", "answer_only", "no_project_settings", "no_project_mcp")
PATHS = ("files_under", "read_only_paths", "denied_paths")
HOSTS = ("network",)
WORDS = ("sandboxed_scripts", "only_skills")

#: The keys a request may have: one it does not declare is refused, never dropped.
START_KEYS = {"environment", "working_roots", "requirements"}
IMPORT_KEYS = {"session_id", "working_directory", "history", "environment"}
EXPORT_KEYS = {"session_id", "working_directory", "environment", "after", "max_bytes"}

#: How many bytes a page holds when the host gives no bound.
DEFAULT_PAGE_BYTES = 1 << 20

#: The header of the history the echo writes and reads: the standard format's name and the one
#: version the echo knows. A history is this line, then one line per prompt and per update.
HISTORY_FORMAT = "coqui-acp-history"
HISTORY_VERSION = 1
REQUIREMENT_KEYS = {"name", "value"}

#: Statuses, as `guatiao_status` numbers.
OK = 0
BAD_VALUE = 7
INTERNAL = 100

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

#: Every session any connection created: the id reported, a UUID, against the echo's own.
_SESSIONS: Dict[str, Dict[str, Any]] = {}
_SESSIONS_LOCK = threading.Lock()
_NEXT = [1]


def _digest(lines: List[str]) -> str:
    """The digest a header gives its body: FNV-1a over each line and its line feed. Enough to see a
    body changed; not a defence against anyone."""
    value = 0xCBF29CE484222325
    for line in lines:
        for byte in line.encode("utf-8") + b"\n":
            value ^= byte
            value = (value * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"fnv1a64:{value:016x}"


def _history_lines(lines: List[Dict[str, Any]]) -> List[str]:
    """A session's history as lines, the header first, carrying the body's digest."""
    body = [json.dumps(line, separators=(",", ":")) for line in lines]
    header = {"format": HISTORY_FORMAT, "version": HISTORY_VERSION, "digest": _digest(body)}
    return [json.dumps(header, separators=(",", ":"))] + body


def _read_history(history: str) -> List[Dict[str, Any]]:
    """The lines of `history` after its header; an empty history has none."""
    lines = [line for line in history.splitlines() if line.strip()]
    if not lines:
        return []
    try:
        header = json.loads(lines[0])
    except ValueError:
        raise BadValue("the history's header is not JSON") from None
    if (
        not isinstance(header, dict)
        or header.get("format") != HISTORY_FORMAT
        or not isinstance(header.get("version"), int)
        or isinstance(header.get("version"), bool)
    ):
        raise BadValue("the history has no header naming its format and version")
    if header["version"] != HISTORY_VERSION:
        raise Unsupported("the echo reads version 1 of the history format only")
    digest = header.get("digest")
    if digest is not None and digest != _digest(lines[1:]):
        raise BadValue("the history's body does not match its header's digest")
    read = []
    for line in lines[1:]:
        try:
            value = json.loads(line)
        except ValueError:
            raise BadValue("a history line is not JSON") from None
        if not isinstance(value, dict) or value.get("method") not in ("session/prompt", "session/update"):
            raise BadValue("a history line is neither a prompt nor an update")
        read.append(value)
    return read


def _keep(session: str, line: Dict[str, Any]) -> None:
    """Adds one line to the history of `session`, when the echo keeps it."""
    with _SESSIONS_LOCK:
        kept = _SESSIONS.get(session)
        if kept is not None:
            kept["lines"].append(line)


def _new_session(session: str, lines: List[Dict[str, Any]]) -> None:
    """Keeps `session`, mapped to an id of the echo's own; the caller holds the lock."""
    _SESSIONS[session] = {"own": f"python-echo-{_NEXT[0]}", "lines": lines}
    _NEXT[0] += 1


def _parts(path: str, windows: bool) -> Optional[List[str]]:
    """The path's parts, the first being where it is rooted, as `WorkingRoots` reads them; None for a
    path that is not absolute or has a `.` or `..` part."""
    if windows:
        seps = r"[\\/]"
        if len(path) >= 2 and path[0] in "\\/" and path[1] in "\\/":
            split = re.split(seps, path[2:], maxsplit=2)
            if len(split) < 2 or not split[0] or not split[1] or split[0] in ("?", "."):
                return None
            root = f"//{split[0]}/{split[1]}".lower()
            rest = split[2] if len(split) > 2 else ""
        elif len(path) >= 3 and path[0].isascii() and path[0].isalpha() and path[1] == ":" and path[2] in "\\/":
            root, rest = path[0].lower() + ":", path[3:]
        else:
            return None
    else:
        if not path.startswith("/"):
            return None
        seps, root, rest = "/", "", path[1:]
    parts = [root]
    for part in re.split(seps, rest):
        if part == "":
            continue
        if part in (".", ".."):
            return None
        parts.append(part.lower() if windows else part)
    return parts


def _plain_absolute(path: str) -> bool:
    return _parts(path, False) is not None or _parts(path, True) is not None


def _resolved(path: str) -> str:
    resolved = os.path.realpath(path)
    if resolved.startswith("\\\\?\\UNC\\"):
        return "\\\\" + resolved[8:]
    if resolved.startswith("\\\\?\\"):
        return resolved[4:]
    return resolved


def _admits(path: str, roots: List[str]) -> Optional[str]:
    """Why `path` is not within `roots`, compared as `WorkingRoots` compares them; None when it is."""
    if not roots:
        return None
    windows = os.name == "nt"

    def inside(parts: List[str], root: str) -> bool:
        root_parts = _parts(root, windows)
        return root_parts is not None and parts[: len(root_parts)] == root_parts

    parts = _parts(path, windows)
    if parts is None:
        return "the path is not absolute, or has a `.` or `..` part"
    if not any(inside(parts, root) for root in roots):
        return "the path is outside the working roots"
    resolved = _parts(_resolved(path), windows)
    if resolved is None or not any(inside(resolved, _resolved(root)) for root in roots):
        return "a link in the path leads outside the working roots"
    return None


def _host_entry(text: str) -> bool:
    return bool(text) and "://" not in text and "/" not in text and not any(c.isspace() or not c.isprintable() for c in text)


def _read_requirements(requirements: Any) -> List[str]:
    """The names of `requirements`, each read as `Narrowing::read_all` reads it; raises BadValue for
    one it cannot read. A name with no fixed meaning is returned, to be refused."""
    if not isinstance(requirements, list):
        raise BadValue("the requirements are not a list")
    names: List[str] = []
    for requirement in requirements:
        if not isinstance(requirement, dict) or not set(requirement) <= REQUIREMENT_KEYS:
            raise BadValue("a requirement has a key it does not declare")
        name = requirement.get("name")
        if not isinstance(name, str):
            raise BadValue("a requirement's name is not text")
        if name in names:
            raise BadValue(f"requirement `{name}` is named twice")
        names.append(name)
        value = requirement.get("value")
        if name in NO_VALUE:
            if value is not None:
                raise BadValue(f"requirement `{name}`: this requirement takes no value")
            continue
        check = _plain_absolute if name in PATHS else _host_entry if name in HOSTS else bool if name in WORDS else None
        if check is None:
            continue
        if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
            raise BadValue(f"requirement `{name}`: this requirement takes a list of texts")
        if not all(check(item) and not any(not c.isprintable() for c in item) for item in value):
            raise BadValue(f"requirement `{name}`: an entry is not one this requirement takes")
    return names


def _refusal(requirements: Any) -> Optional[str]:
    for name in _read_requirements(requirements):
        if name not in HOLDS:
            return f"the echo cannot hold `{name}`"
    return None


class _Queue:
    """What the connection and its thread share."""

    def __init__(self) -> None:
        self.cond = threading.Condition()
        self.items: Deque[bytes] = deque()
        #: Either side has ended the connection: `send` answers GONE.
        self.closed = False
        #: The status it ends with, once one side has given it.
        self.status: Optional[int] = None

    def end(self, status: int) -> None:
        """Ends the connection with `status` unless it has ended, dropping what is still queued."""
        with self.cond:
            self.closed = True
            if self.status is None:
                self.status = status
            self.items.clear()
            self.cond.notify_all()


class Connection(Channel):
    """The host's end. The thread holds the queue, not this object, so dropping it closes."""

    def __init__(self, queue: _Queue) -> None:
        self._queue = queue

    def send(self, bytes: bytes) -> None:
        q = self._queue
        if len(bytes) > MAX_MESSAGE_BYTES:
            q.end(BAD_VALUE)
            raise BadValue("the message is larger than the echo takes")
        with q.cond:
            while True:
                if q.closed:
                    raise Gone("the connection is closed")
                if len(q.items) < QUEUE:
                    break
                q.cond.wait()
            q.items.append(bytes)
            q.cond.notify_all()

    def close(self, status: int) -> None:
        q = self._queue
        with q.cond:
            if q.closed:
                return
            q.closed = True
            q.status = status
            q.cond.notify_all()

    def __del__(self) -> None:
        self.close(OK)


def _result(id: Any, result: Dict[str, Any]) -> Dict[str, Any]:
    return {"jsonrpc": "2.0", "id": id, "result": result}


def _error(id: Any, code: int, message: str) -> Dict[str, Any]:
    return {"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}


def _finish(session: str, prompt: Any, said: Optional[str], stop: str) -> List[Dict[str, Any]]:
    """A turn's end: what the agent says, if anything, kept in the session's history, then the
    prompt's answer."""
    messages: List[Dict[str, Any]] = []
    if said is not None:
        _keep(session, {
            "method": "session/update",
            "params": {"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": said}}},
        })
        messages.append({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session,
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": said}},
            },
        })
    messages.append(_result(prompt, {"stopReason": stop}))
    return messages


class _Refused(Exception):
    """A request answered -32602, with why."""


class _Agent:
    """The ACP side of one connection: what each message from the client is answered with."""

    def __init__(self, roots: List[str]) -> None:
        self.roots = roots
        self.open: List[str] = []
        self.reads_files = False
        #: The turn running in each session: (kind, prompt id, the agent's own request id).
        self.turns: Dict[str, Tuple[str, Any, Optional[str]]] = {}
        self.asked = 0

    def answer(self, data: bytes) -> Tuple[List[Dict[str, Any]], bool]:
        """The messages to send back, and whether the connection ends after them."""
        try:
            message = json.loads(data)
        except ValueError:
            return [_error(None, -32700, "not JSON")], False
        if not isinstance(message, dict):
            return [_error(None, -32600, "not one JSON-RPC message")], False
        id = message.get("id")
        if "method" not in message:
            return self.answered(message), False
        method = message["method"]
        if not isinstance(method, str):
            return [_error(id, -32600, "the method is not text")], False
        params = message.get("params", {})
        if "id" not in message:
            return self.notified(method, params), False
        if not isinstance(params, dict):
            return [_error(id, -32602, "the params are not an object")], False
        try:
            return self.requested(method, id, params)
        except _Refused as why:
            return [_error(id, -32602, str(why))], False

    def requested(self, method: str, id: Any, params: Dict[str, Any]) -> Tuple[List[Dict[str, Any]], bool]:
        def text(key: str) -> str:
            value = params.get(key)
            if not isinstance(value, str):
                raise _Refused(f"`{key}` is not text")
            return value

        if method == "initialize":
            capabilities = params.get("clientCapabilities")
            fs = capabilities.get("fs") if isinstance(capabilities, dict) else None
            self.reads_files = isinstance(fs, dict) and fs.get("readTextFile") is True
            return [_result(id, {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": True,
                    "promptCapabilities": {},
                    "mcpCapabilities": {"http": False, "sse": False},
                },
                "authMethods": [],
                "agentInfo": {"name": "acp_echo_harness", "title": "Echo, in Python", "version": __version__},
            })], False
        if method == "session/new":
            self.admit(text("cwd"), params)
            session = str(uuid.uuid4())
            with _SESSIONS_LOCK:
                _new_session(session, [])
            self.open.append(session)
            return [_result(id, {"sessionId": session})], False
        if method in ("session/load", "session/resume"):
            self.admit(text("cwd"), params)
            session = text("sessionId")
            with _SESSIONS_LOCK:
                kept = _SESSIONS.get(session)
                if kept is None:
                    raise _Refused("no session has this id")
                lines = list(kept["lines"])
            self.open.append(session)
            # `session/load` replays what the session said; `session/resume` does not.
            replay = [
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {"sessionId": session, "update": line["params"]["update"]},
                }
                for line in lines
                if method == "session/load" and line.get("method") == "session/update"
            ]
            return replay + [_result(id, {})], False
        if method == "session/prompt":
            session = text("sessionId")
            blocks = params.get("prompt")
            if not isinstance(blocks, list):
                raise _Refused("`prompt` is not a list")
            if session not in self.open:
                raise _Refused("no session with this id is open here")
            _keep(session, {"method": "session/prompt", "params": {"prompt": blocks}})
            said = "".join(b["text"] for b in blocks if isinstance(b, dict) and isinstance(b.get("text"), str))
            return self.prompted(id, session, said)
        return [_error(id, -32601, "method not found")], False

    def admit(self, cwd: str, params: Dict[str, Any]) -> None:
        """Refuses a session whose directories are not all inside the working roots."""
        directories = [cwd]
        more = params.get("additionalDirectories", [])
        if not isinstance(more, list) or not all(isinstance(d, str) for d in more):
            raise _Refused("`additionalDirectories` is not a list of texts")
        for directory in directories + more:
            why = _admits(directory, self.roots)
            if why is not None:
                raise _Refused(why)

    def prompted(self, id: Any, session: str, said: str) -> Tuple[List[Dict[str, Any]], bool]:
        if said == "wait":
            self.turns[session] = ("wait", id, None)
            return [], False
        if said == "ask":
            self.asked += 1
            request = f"python-echo-ask-{self.asked}"
            self.turns[session] = ("ask", id, request)
            return [{
                "jsonrpc": "2.0",
                "id": request,
                "method": "session/request_permission",
                "params": {
                    "sessionId": session,
                    "toolCall": {"toolCallId": request, "title": "Echo asks"},
                    "options": [
                        {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                    ],
                },
            }], False
        if said.startswith("read "):
            if not self.reads_files:
                return _finish(session, id, "echo: no fs", "end_turn"), False
            self.asked += 1
            request = f"python-echo-read-{self.asked}"
            self.turns[session] = ("read", id, request)
            return [{
                "jsonrpc": "2.0",
                "id": request,
                "method": "fs/read_text_file",
                "params": {"sessionId": session, "path": said[len("read "):]},
            }], False
        return _finish(session, id, f"echo: {said}", "end_turn"), said == "close"

    def notified(self, method: str, params: Any) -> List[Dict[str, Any]]:
        if method != "session/cancel" or not isinstance(params, dict):
            return []
        session = params.get("sessionId")
        turn = self.turns.pop(session, None) if isinstance(session, str) else None
        return _finish(session, turn[1], None, "cancelled") if turn else []

    def answered(self, message: Dict[str, Any]) -> List[Dict[str, Any]]:
        """The client's answer to one of the agent's own requests."""
        found = [s for s, (_, _, request) in self.turns.items() if request is not None and request == message.get("id")]
        if not found:
            return []
        session = found[0]
        kind, prompt, _ = self.turns.pop(session)
        result = message.get("result")
        result = result if isinstance(result, dict) else {}
        if kind == "ask":
            outcome = result.get("outcome")
            outcome = outcome if isinstance(outcome, dict) else {}
            if outcome.get("outcome") == "cancelled":
                return _finish(session, prompt, None, "cancelled")
            allowed = outcome.get("outcome") == "selected" and outcome.get("optionId") == "allow"
            return _finish(session, prompt, "echo: allowed" if allowed else "echo: refused", "end_turn")
        content = result.get("content")
        said = f"echo: {content}" if isinstance(content, str) else "echo: refused"
        return _finish(session, prompt, said, "end_turn")

    def end_every_turn(self) -> List[Dict[str, Any]]:
        """Ends every turn still running, as cancelled: the connection is ending."""
        turns, self.turns = self.turns, {}
        return [m for session, (_, prompt, _) in turns.items() for m in _finish(session, prompt, None, "cancelled")]


def _deliver(sink: ChannelSink, spawner: ProcessSpawner, q: _Queue, agent: _Agent) -> None:
    """The connection's thread, and the only caller of the sink."""

    def send(answers: List[Dict[str, Any]]) -> Optional[int]:
        for answer in answers:
            try:
                sink.receive(json.dumps(answer).encode("utf-8"))
            except Exception as e:  # any refusal ends the connection, once
                return int(getattr(e, "status", INTERNAL))
        return None

    status = OK
    try:
        while True:
            with q.cond:
                while not q.items and not q.closed:
                    q.cond.wait()
                message: Optional[bytes] = q.items.popleft() if q.items else None
                q.cond.notify_all()
            if message is None:
                # The host closed: every turn still running ends, answered, before `closed`.
                refused = send(agent.end_every_turn())
                status = refused if refused is not None else (q.status if q.status is not None else OK)
                break
            try:
                answers, then_close = agent.answer(message)
            except Exception:  # a defect here answers the request it was handling, then goes on
                request = None
                try:
                    request = json.loads(message).get("id")
                except Exception:
                    pass
                answers, then_close = [_error(request, -32603, "the echo failed")], False
            refused = send(answers)
            if refused is not None:
                status = refused
                q.end(refused)
                break
            if then_close:
                q.end(OK)
                send(agent.end_every_turn())
                break
    except BaseException:
        status = INTERNAL
        q.end(INTERNAL)
    finally:
        try:
            spawner.destroy()
            sink.closed(status)
        finally:
            sink.destroy()


class EchoHarness(provide.Provider, AcpHarness, display_name="Echo harness, in Python"):
    """Starts an echoing ACP connection per call."""

    def describe(self) -> Dict[str, Any]:
        return {
            "title": "Echo, in Python",
            "versions": [1],
            "takes": {"load": True, "resume": True, "additional_directories": True},
        }

    def start(self, request: Ref, spawner: ProcessSpawner, sink: ChannelSink) -> Channel:
        try:
            asked = request.to_python()
            if not isinstance(asked, dict) or not set(asked) <= START_KEYS:
                raise BadValue("the request has a key the echo does not know")
            refusal = _refusal(asked.get("requirements", []))
            if refusal is not None:
                raise Unsupported(refusal)
            roots = asked.get("working_roots", [])
            if not isinstance(roots, list) or not all(isinstance(r, str) for r in roots):
                raise BadValue("the working roots are not a list of texts")
        except BaseException:
            # A refused start still lets go of what it was handed.
            spawner.destroy()
            sink.destroy()
            raise
        q = _Queue()
        threading.Thread(
            target=_deliver,
            args=(sink, spawner, q, _Agent(list(roots))),
            name="acp_echo_harness",
            daemon=True,
        ).start()
        return Connection(q)

    def refusal(self, requirements: Ref, environment: Ref) -> Optional[str]:
        return _refusal(requirements.to_python())

    def program(self) -> Optional[str]:
        return None

    def import_session(self, session: Ref, spawner: ProcessSpawner) -> Dict[str, Any]:
        spawner.destroy()
        asked = session.to_python()
        if not isinstance(asked, dict) or not set(asked) <= IMPORT_KEYS:
            raise BadValue("the import has a key the echo does not know")
        id = asked.get("session_id")
        if not (isinstance(id, str) and UUID.fullmatch(id)):
            raise BadValue("the session id is not a UUID")
        history = asked.get("history", "")
        if not isinstance(history, str):
            raise BadValue("the history is not text")
        lines = _read_history(history)
        with _SESSIONS_LOCK:
            if id in _SESSIONS:
                raise BadValue("the id names a session already")
            _new_session(id, lines)
        report: Dict[str, Any] = {"losses": [], "compacted": False}
        if history.strip():
            report["seeded_from"] = {"line": 2}
        return report

    def export_session(self, export: Ref, spawner: ProcessSpawner) -> Dict[str, Any]:
        """A position is `<line>:<digest>`: the next line to give, and the digest of the body when
        the first page was given, so a session that has changed since refuses it."""
        spawner.destroy()
        asked = export.to_python()
        if not isinstance(asked, dict) or not set(asked) <= EXPORT_KEYS:
            raise BadValue("the export has a key the echo does not know")
        with _SESSIONS_LOCK:
            kept = _SESSIONS.get(asked.get("session_id"))
            if kept is None:
                raise NotFound("no session has this id")
            lines = _history_lines(list(kept["lines"]))
        digest = _digest(lines[1:])
        after = asked.get("after")
        start = 0
        if after is not None:
            line, _, given = str(after).partition(":")
            if given != digest or not line.isdigit() or not 1 <= int(line) < len(lines):
                raise BadValue("the position is not one the echo gave, or the session has changed since")
            start = int(line)
        limit = asked.get("max_bytes", DEFAULT_PAGE_BYTES)
        text, end, size = "", start, 0
        while end < len(lines):
            line = lines[end] + "\n"
            width = len(line.encode("utf-8"))
            if end > start and size + width > limit:
                break
            text += line
            size += width
            end += 1
        page: Dict[str, Any] = {"text": text}
        if end < len(lines):
            page["next"] = f"{end}:{digest}"
        return page

    def models(self, environment: Ref) -> Dict[str, Any]:
        return {"known": True, "models": [{"id": "echo"}]}
