"""An ACP agent that answers each prompt with its text, as an `acp_harness` library written in Python.

The module is the library: its id is its name, its version `__version__`, its providers the classes in
`__all__`. It is the contract agent of `coqui_acp_api::contract`, speaking ACP itself, one
JSON-RPC message per channel message, and keeps every rule of the channel: `send` waits for room in a
queue of a few messages and `close` wakes it; one thread of its own answers, in order, and is the only
caller of the sink; a sink that refuses ends the connection. It starts no program of its own: on an
account it starts a notional one through the spawner, to show the specification a real provider's would
carry.

It takes accounts of two kinds besides the harness's own: `echo-login`, a login kept in a file of the
account's directory, and `echo-key`, a key in a variable. It signs in, renews and watches nothing: that is
the echo account provider's.

With the setting `usage = "scripted"` it also reports usage, as the contract documents: `quota P`
reports two buckets, `limit` refuses the turn with a limit hit, and every turn spends fixed tokens.
"""

from __future__ import annotations

import json
import os
import re
import threading
import time
import uuid
from collections import deque
from dataclasses import dataclass
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
START_KEYS = {"environment", "working_roots", "requirements", "account", "cache_lifetime"}
IMPORT_KEYS = {"session_id", "working_directory", "history", "environment", "account"}
EXPORT_KEYS = {"session_id", "working_directory", "environment", "account", "after", "max_bytes"}
QUERY_KEYS = {"environment", "account"}

#: The usage extension's names, as `coqui_acp_api::usage` spells them.
USAGE_CAPABILITY = "dev.boriken.coqui.acp/usage"
USAGE_METHOD = "_coqui_acp/usage"
USAGE_NOTIFICATION = "_coqui_acp/usage_update"
LIMIT_META_KEY = "dev.boriken.coqui.acp/limit"

#: The tokens every turn of a scripted echo spends: input, output, cache read, cache write.
SPENT = (100, 40, 20, 10)

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


class _Reports:
    """The newest report a connection of one instance made, by the account it ran on ("" for none)."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.held: Dict[str, Dict[str, Any]] = {}


def _bucket(id: str, **fields: Any) -> Dict[str, Any]:
    """A bucket with every key the model has, `fields` over nothing known."""
    bucket: Dict[str, Any] = {
        "id": id,
        "label": None,
        "models": {"kind": "unknown"},
        "unit": "permille",
        "used": None,
        "limit": None,
        "remaining": None,
        "refill": {"kind": "unknown"},
        "status": "unknown",
        "observed_at": 0,
        "source": "reported",
        "evidence": None,
    }
    bucket.update(fields)
    return bucket


class _Usage:
    """The usage of one connection of a scripted echo."""

    def __init__(self, account: Optional[str], reports: "_Reports") -> None:
        self.reports = reports
        self.report: Dict[str, Any] = {
            "supported": True,
            "unsupported_because": None,
            "sources": ["reported"],
            "account": account,
            "revision": 0,
            "observed_at": 0,
            "buckets": [],
            "limit": None,
            "spend": None,
        }
        self.spends: Dict[str, Dict[str, Any]] = {}
        #: Whether the client asked for `_coqui_acp/usage_update`.
        self.notify = False

    def snapshot(self, session: Optional[str]) -> Dict[str, Any]:
        """The report, with `session`'s spend when one is named."""
        report = dict(self.report)
        report["buckets"] = [dict(bucket) for bucket in self.report["buckets"]]
        report["spend"] = dict(self.spends[session]) if session in self.spends else None
        return report

    def changed(self, session: Optional[str]) -> List[Dict[str, Any]]:
        """Something changed: a new revision, kept for the kind's `usage`, and the notification when the
        client asked."""
        self.report["revision"] += 1
        self.report["observed_at"] = max((b["observed_at"] for b in self.report["buckets"]), default=0)
        with self.reports.lock:
            self.reports.held[self.report["account"] or ""] = self.snapshot(None)
        if not self.notify:
            return []
        return [{
            "jsonrpc": "2.0",
            "method": USAGE_NOTIFICATION,
            "params": {"sessionId": session, "report": self.snapshot(session)},
        }]

    def quota(self, permille: int, session: str) -> List[Dict[str, Any]]:
        """`quota P`: the five-hour window used `P` thousandths, and a request bucket that refills
        continuously."""
        at = int(time.time())
        five = _bucket(
            "five_hour",
            label="5 hours",
            models={"kind": "all"},
            used=permille,
            limit=1000,
            refill={"kind": "fixed_window", "length": [5 * 3600, 0], "resets_at": at + 5 * 3600},
            status="exhausted" if permille >= 1000 else "ok",
            observed_at=at,
        )
        rpm = _bucket(
            "rpm",
            models={"kind": "all"},
            unit="requests",
            limit=60,
            remaining=60,
            refill={"kind": "token_bucket", "amount": 60, "per": [60, 0], "max": 60},
            status="ok",
            observed_at=at,
        )
        self.report["buckets"] = [five, rpm]
        self.report["limit"] = None
        return self.changed(session)

    def limit(self, session: str) -> Tuple[List[Dict[str, Any]], Dict[str, Any]]:
        """`limit`: the five-hour window is used up and the turn is refused."""
        at = int(time.time())
        hit = {
            "bucket": "five_hour",
            "resets_at": at + 3600,
            "retry_after": [60, 0],
            "source": "reported",
            "text": "usage limit reached",
            "observed_at": at,
        }
        for bucket in self.report["buckets"]:
            if bucket["id"] == "five_hour":
                bucket.update(status="exhausted", used=1000, observed_at=at)
                break
        else:
            self.report["buckets"].append(_bucket("five_hour", used=1000, limit=1000, status="exhausted", observed_at=at))
        self.report["limit"] = hit
        return self.changed(session), hit

    def spent(self, session: str) -> List[Dict[str, Any]]:
        """A turn ended: its fixed spend is added to the session's totals."""
        spend = self.spends.setdefault(session, {
            "session_id": session,
            "input": 0,
            "output": 0,
            "cache_read": 0,
            "cache_write": 0,
            "reasoning": None,
            "cost_micro_usd": None,
            "by_model": [],
        })
        for key, amount in zip(("input", "output", "cache_read", "cache_write"), SPENT):
            spend[key] += amount
        return self.changed(session)


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
        #: The status it ends with, and why, once one side has given them.
        self.status: Optional[Tuple[int, str]] = None

    def end(self, status: int, reason: str = "") -> None:
        """Ends the connection with `status` and why, unless it has ended, dropping what is still
        queued."""
        with self.cond:
            self.closed = True
            if self.status is None:
                self.status = (status, reason)
            self.items.clear()
            self.cond.notify_all()


class Connection(Channel):
    """The host's end. The thread holds the queue, not this object, so dropping it closes."""

    def __init__(self, queue: _Queue) -> None:
        self._queue = queue

    def send(self, bytes: bytes) -> None:
        q = self._queue
        if len(bytes) > MAX_MESSAGE_BYTES:
            why = "the message is larger than the echo takes"
            q.end(BAD_VALUE, why)
            raise BadValue(why)
        with q.cond:
            while True:
                if q.closed:
                    raise Gone("the connection is closed")
                if len(q.items) < QUEUE:
                    break
                q.cond.wait()
            q.items.append(bytes)
            q.cond.notify_all()

    def close(self, status: int, reason: str) -> None:
        q = self._queue
        with q.cond:
            if q.closed:
                return
            q.closed = True
            q.status = (status, reason)
            q.cond.notify_all()

    def __del__(self) -> None:
        self.close(OK, "")


def _result(id: Any, result: Dict[str, Any]) -> Dict[str, Any]:
    return {"jsonrpc": "2.0", "id": id, "result": result}


def _error(id: Any, code: int, message: str) -> Dict[str, Any]:
    return {"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}


def _finish(
    session: str, prompt: Any, said: Optional[str], stop: str, meta: Optional[Dict[str, Any]] = None
) -> List[Dict[str, Any]]:
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
    answer: Dict[str, Any] = {"stopReason": stop}
    if meta is not None:
        answer["_meta"] = meta
    messages.append(_result(prompt, answer))
    return messages


class _Refused(Exception):
    """A request answered -32602, with why."""


class _Agent:
    """The ACP side of one connection: what each message from the client is answered with."""

    def __init__(self, roots: List[str], usage: Optional[_Usage] = None) -> None:
        self.roots = roots
        self.usage = usage
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
            agent_capabilities: Dict[str, Any] = {
                "loadSession": True,
                "promptCapabilities": {},
                "mcpCapabilities": {"http": False, "sse": False},
            }
            if self.usage is not None:
                meta = capabilities.get("_meta") if isinstance(capabilities, dict) else None
                asked = meta.get(USAGE_CAPABILITY) if isinstance(meta, dict) else None
                asked = asked if isinstance(asked, dict) else {}
                self.usage.notify = asked.get("notify") is True
                if self.usage.report["account"] is None and isinstance(asked.get("account"), str):
                    self.usage.report["account"] = asked["account"]
                agent_capabilities["_meta"] = {
                    USAGE_CAPABILITY: {"version": 1, "sources": ["reported"], "spend": True}
                }
            return [_result(id, {
                "protocolVersion": 1,
                "agentCapabilities": agent_capabilities,
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
        if method == USAGE_METHOD and self.usage is not None:
            session = params.get("sessionId")
            if session is not None and (not isinstance(session, str) or session not in self.open):
                raise _Refused("no such session is open here")
            return [_result(id, self.usage.snapshot(session))], False
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
                return self.finish(session, id, "echo: no fs", "end_turn"), False
            self.asked += 1
            request = f"python-echo-read-{self.asked}"
            self.turns[session] = ("read", id, request)
            return [{
                "jsonrpc": "2.0",
                "id": request,
                "method": "fs/read_text_file",
                "params": {"sessionId": session, "path": said[len("read "):]},
            }], False
        if self.usage is not None:
            if said.startswith("quota ") and said[6:].strip().isdigit():
                answers = self.usage.quota(int(said[6:].strip()), session)
                return answers + self.finish(session, id, f"echo: {said}", "end_turn"), False
            if said == "limit":
                answers, hit = self.usage.limit(session)
                return answers + self.finish(session, id, "echo: limit", "refusal", {LIMIT_META_KEY: hit}), False
        return self.finish(session, id, f"echo: {said}", "end_turn"), said == "close"

    def finish(
        self, session: str, prompt: Any, said: Optional[str], stop: str, meta: Optional[Dict[str, Any]] = None
    ) -> List[Dict[str, Any]]:
        """A turn's end; a scripted echo spends its fixed tokens first when the turn ran."""
        spent = self.usage.spent(session) if self.usage is not None and stop in ("end_turn", "refusal") else []
        return spent + _finish(session, prompt, said, stop, meta)

    def notified(self, method: str, params: Any) -> List[Dict[str, Any]]:
        if method != "session/cancel" or not isinstance(params, dict):
            return []
        session = params.get("sessionId")
        turn = self.turns.pop(session, None) if isinstance(session, str) else None
        return self.finish(session, turn[1], None, "cancelled") if turn else []

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
                return self.finish(session, prompt, None, "cancelled")
            allowed = outcome.get("outcome") == "selected" and outcome.get("optionId") == "allow"
            return self.finish(session, prompt, "echo: allowed" if allowed else "echo: refused", "end_turn")
        content = result.get("content")
        said = f"echo: {content}" if isinstance(content, str) else "echo: refused"
        return self.finish(session, prompt, said, "end_turn")

    def end_every_turn(self) -> List[Dict[str, Any]]:
        """Ends every turn still running, as cancelled: the connection is ending."""
        turns, self.turns = self.turns, {}
        return [m for session, (_, prompt, _) in turns.items() for m in self.finish(session, prompt, None, "cancelled")]


def _deliver(sink: ChannelSink, hold: _Hold, q: _Queue, agent: _Agent) -> None:
    """The connection's thread, and the only caller of the sink."""

    def send(answers: List[Dict[str, Any]]) -> Optional[Tuple[int, str]]:
        for answer in answers:
            try:
                sink.receive(json.dumps(answer).encode("utf-8"))
            except Exception as e:  # any refusal ends the connection, once
                return int(getattr(e, "status", INTERNAL)), str(e)
        return None

    status: Tuple[int, str] = (OK, "")
    try:
        while True:
            with q.cond:
                while not q.items and not q.closed:
                    q.cond.wait()
                message: Optional[bytes] = q.items.popleft() if q.items else None
                q.cond.notify_all()
            if message is None:
                # The host closed: every turn still running ends, answered, before `closed`, which
                # passes the host's reason on.
                refused = send(agent.end_every_turn())
                status = refused if refused is not None else (q.status if q.status is not None else (OK, ""))
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
                q.end(*refused)
                break
            if then_close:
                q.end(OK)
                send(agent.end_every_turn())
                break
    except BaseException as e:
        status = (INTERNAL, f"the echo failed: {type(e).__name__}")
        q.end(*status)
    finally:
        try:
            hold.release()
            sink.closed(status[0], status[1])
        finally:
            sink.destroy()


# ---------------------------------------------------------------------------------------------
# Accounts
# ---------------------------------------------------------------------------------------------

HARNESS_LOGIN = "harness-login"
#: The kind with a file credential: the login the echo keeps in `auth.json` of the account's
#: directory.
FILE_KIND = "echo-login"
#: The kind with a credential in a variable.
KEY_KIND = "echo-key"
FILE = "auth.json"
FILE_KEY = "auth_file"
#: The variables the echo reads for a credential.
VARIABLES = ["ECHO_API_KEY", "ECHO_TOKEN"]
ACCOUNT_ID_MAX = 128
GRANT_KEYS = {"id", "kind", "settings", "credentials", "state_dir", "revision", "provides"}
REF_KEYS = {"id", "kind", "settings", "state_dir", "revision", "provides"}
#: What the echo starts on besides its own kinds: a key of the standard `echo-api`, run as `echo-key`.
ACCEPTS = [
    {
        "standard": "echo-api",
        "slots": [
            {
                "credential": "api_key",
                "kind": "echo-key",
                "variable": "ECHO_API_KEY",
                "file": None,
                "base_url_setting": "base_url",
                "base_url_variable": None,
                "dir_variable": None,
                "settings": {},
            }
        ],
    }
]
MODELS_KEYS = {"environment", "account"}
CACHE_LIFETIMES = ("default", "short", "long")


def _py(value: Any) -> Any:
    """A value a host object answered, as plain Python."""
    return value.to_python() if hasattr(value, "to_python") else value


def _credential() -> Dict[str, Any]:
    return {"type": "string", "writeOnly": True, "x-sensitive": True}


def _catalog() -> Dict[str, Any]:
    """What the echo takes as an account."""
    url = {"type": "string", "format": "uri"}
    endpoint = [{"key": "base_url", "purpose": "model_api", "default": "https://echo.example"}]
    kinds = [
        {
            "id": HARNESS_LOGIN,
            "title": "The harness's own login",
            "description": "Nothing is handed over and nothing is cleared: the harness uses the login it "
            "already has where it runs. The account counts use and limits.",
            "settings": {},
            "endpoints": [],
            "rotates": True,
            "history_portable": False,
            "state_dir": False,
            "renews": False,
            "notices": [],
            "provides": [],
        },
        {
            "id": FILE_KIND,
            "title": "Echo login",
            "description": "A login the echo keeps in a file of its own directory.",
            "settings": {
                "type": "object",
                "properties": {FILE_KEY: _credential(), "base_url": url},
                "required": [FILE_KEY],
                "additionalProperties": False,
            },
            "endpoints": endpoint,
            "rotates": True,
            "history_portable": False,
            "state_dir": True,
            "renews": True,
            "notices": ["the login file can be read by what the agent runs"],
            "provides": ["echo"],
        },
        {
            "id": KEY_KIND,
            "title": "Echo key",
            "description": "A key the echo takes in a variable.",
            "settings": {
                "type": "object",
                "properties": {"api_key": _credential(), "base_url": url},
                "required": ["api_key"],
                "additionalProperties": False,
            },
            "endpoints": endpoint,
            "rotates": False,
            "history_portable": True,
            "state_dir": False,
            "renews": False,
            "notices": [],
            "provides": ["echo-api"],
        },
    ]
    return {"kinds": kinds, "logins": [], "credential_variables": list(VARIABLES), "accepts": ACCEPTS}


def _is_account_id(text: Any) -> bool:
    return (
        isinstance(text, str)
        and 1 <= len(text.encode("utf-8")) <= ACCOUNT_ID_MAX
        and not any(ord(c) < 32 or 127 <= ord(c) < 160 for c in text)
    )


def _read_account(asked: Any, keys: set) -> None:
    """Checks an account's keys, id, kind and settings, without touching a spawner."""
    if not isinstance(asked, dict) or not set(asked) <= keys:
        raise BadValue("the account has a key the echo does not know")
    if not _is_account_id(asked.get("id")):
        raise BadValue("an account id is 1 to 128 bytes with no control character")
    provides = asked.get("provides", [])
    if not isinstance(provides, list) or not all(isinstance(p, str) for p in provides):
        raise BadValue("provides is not a list of standards")
    kind = asked.get("kind")
    if kind not in [k["id"] for k in _catalog()["kinds"]]:
        accepted = [a["standard"] for a in ACCEPTS]
        if not any(p in accepted for p in provides):
            raise BadValue(
                "the echo takes no account of that kind: it accepts "
                + (", ".join(accepted) or "none")
                + ", and the account provides "
                + (", ".join(provides) or "none")
            )
        return
    settings = asked.get("settings", {})
    if not isinstance(settings, dict):
        raise BadValue("the settings are not an object")
    allowed = () if kind == HARNESS_LOGIN else ("base_url",)
    for key, value in settings.items():
        url = isinstance(value, str) and (value.startswith("https://") or value.startswith("http://"))
        if key not in allowed or (key == "base_url" and not url):
            raise BadValue("the settings do not fit the kind's schema")


def _resolve(grant: Dict[str, Any]) -> Dict[str, Any]:
    """The grant of the echo's own kind that applies a grant of an accepted standard."""
    accepted = next(a for a in ACCEPTS if a["standard"] in grant.get("provides", []))
    credentials = grant.get("credentials", {})
    slot = next((s for s in accepted["slots"] if sorted(credentials) == [s["credential"]]), None)
    if slot is None:
        raise BadValue("the credential is not one the echo takes for " + accepted["standard"])
    settings = {}
    for name, value in grant.get("settings", {}).items():
        if name != "base_url" or slot["base_url_setting"] is None:
            raise BadValue("the settings do not fit the standard")
        settings[slot["base_url_setting"]] = value
    resolved = dict(grant)
    resolved["kind"] = slot["kind"]
    resolved["settings"] = settings
    return resolved


def _read_grant(grant: Any, spawner: ProcessSpawner) -> Dict[str, Any]:
    """Checks a grant for the echo and what it needs of `spawner`."""
    _read_account(grant, GRANT_KEYS)
    if grant["kind"] not in [k["id"] for k in _catalog()["kinds"]]:
        grant = _resolve(grant)
    credentials = grant.get("credentials", {})
    wanted = {FILE_KIND: [FILE_KEY], KEY_KIND: ["api_key"]}.get(grant["kind"], [])
    if (
        not isinstance(credentials, dict)
        or sorted(credentials) != sorted(wanted)
        or not all(isinstance(credentials[k], str) and credentials[k] for k in wanted)
    ):
        raise BadValue("a credential is missing, or one the kind does not take was given")
    if grant["kind"] == FILE_KIND:
        if grant.get("state_dir") is None:
            raise BadValue("this kind needs a directory and none was granted")
        if not _py(spawner.describe()).get("files"):
            raise Unsupported("this spawner has no private files, which an echo login needs")
    return grant


def _apply(grant: Any, spawner: ProcessSpawner) -> Dict[str, Any]:
    """Applies `grant` for a call that runs the harness: the credential file of a file kind is written
    into the account's directory."""
    grant = _read_grant(grant, spawner)
    if grant["kind"] == FILE_KIND:
        spawner.write_file(FILE, grant["credentials"][FILE_KEY].encode("utf-8"), True)
    return grant


def _notional(grant: Dict[str, Any]) -> Dict[str, Any]:
    """The program the echo notionally starts on an account: its specification carries what a real
    provider's would, and the suite's spawner records it."""
    spec: Dict[str, Any] = {"program": "echo-agent"}
    if grant["kind"] != HARNESS_LOGIN:
        spec["remove_environment"] = list(_catalog()["credential_variables"])
        if grant["kind"] == KEY_KIND:
            spec["environment"] = {VARIABLES[0]: grant["credentials"]["api_key"]}
    if grant.get("state_dir"):
        spec["cwd"] = grant["state_dir"]
    return spec


class _Hold:
    """What a connection keeps of the objects it was handed, so that everything is let go before
    `closed`."""

    def __init__(self, spawner: ProcessSpawner) -> None:
        self.spawner = spawner
        #: The notional program; killed when the connection ends.
        self.process: Any = None

    def release(self) -> None:
        """Ends the program and lets go of every object."""
        if self.process is not None:
            try:
                self.process.kill()
            except Exception:
                pass
            self.process.destroy()
            self.process = None
        self.spawner.destroy()


@dataclass(frozen=True)
class EchoConfig:
    """How an echo is configured."""

    #: `none`, which reports no usage, or `scripted`, which reports what its prompts script.
    usage: str = "none"


class EchoHarness(provide.Provider, AcpHarness, display_name="Echo harness, in Python", default=True):
    """Starts an echoing ACP connection per call."""

    def __init__(self, config: EchoConfig = EchoConfig()) -> None:
        if config.usage not in ("none", "scripted"):
            raise BadValue("`usage` is `none` or `scripted`")
        self.scripted = config.usage == "scripted"
        self.reports = _Reports()

    def describe(self) -> Dict[str, Any]:
        info: Dict[str, Any] = {
            "title": "Echo, in Python",
            "versions": [1],
            "takes": {"load": True, "resume": True, "additional_directories": True},
        }
        if self.scripted:
            info["usage"] = {"sources": ["reported"], "spend": True}
        return info

    def accounts(self) -> Dict[str, Any]:
        return _catalog()

    def _applied(self, grant: Any, spawner: ProcessSpawner) -> None:
        """Applies `grant`, when there is one, for the length of a call."""
        if grant is not None:
            _apply(grant, spawner)

    def start(self, request: Ref, spawner: ProcessSpawner, sink: ChannelSink) -> Channel:
        # A refusal raises: the binding then destroys the objects it was handed.
        asked = request.to_python()
        if not isinstance(asked, dict) or not set(asked) <= START_KEYS:
            raise BadValue("the request has a key the echo does not know")
        if asked.get("cache_lifetime", "default") not in CACHE_LIFETIMES:
            raise BadValue("the cache lifetime is default, short or long")
        grant = asked.get("account")
        hold = _Hold(spawner)
        if grant is not None:
            grant = _apply(grant, spawner)
            # A notional program, so the specification a real provider's would carry is seen; a
            # spawner that refuses it changes nothing.
            try:
                hold.process = spawner.spawn(_notional(grant))
            except Exception:
                hold.process = None
        refusal = _refusal(asked.get("requirements", []))
        if refusal is not None:
            raise Unsupported(refusal)
        roots = asked.get("working_roots", [])
        if not isinstance(roots, list) or not all(isinstance(r, str) for r in roots):
            raise BadValue("the working roots are not a list of texts")
        account = grant["id"] if grant is not None else None
        q = _Queue()
        usage = _Usage(account, self.reports) if self.scripted else None
        threading.Thread(
            target=_deliver,
            args=(sink, hold, q, _Agent(list(roots), usage)),
            name="acp_echo_harness",
            daemon=True,
        ).start()
        return Connection(q)

    def refusal(self, requirements: Ref, environment: Ref, account: Optional[Ref]) -> Optional[str]:
        named = account.to_python() if account is not None else None
        if named is not None:
            _read_account(named, REF_KEYS)
        return _refusal(requirements.to_python())

    def program(self) -> Optional[str]:
        return None

    def import_session(self, session: Ref, spawner: ProcessSpawner) -> Dict[str, Any]:
        asked = session.to_python()
        try:
            if not isinstance(asked, dict) or not set(asked) <= IMPORT_KEYS:
                raise BadValue("the import has a key the echo does not know")
            self._applied(asked.get("account"), spawner)
        finally:
            spawner.destroy()
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
        asked = export.to_python()
        try:
            if not isinstance(asked, dict) or not set(asked) <= EXPORT_KEYS:
                raise BadValue("the export has a key the echo does not know")
            self._applied(asked.get("account"), spawner)
        finally:
            spawner.destroy()
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
            if size + width > limit:
                if end == start:
                    raise BadValue(f"line too large: {width} bytes, more than max_bytes {limit}")
                break
            text += line
            size += width
            end += 1
        page: Dict[str, Any] = {"text": text}
        if end < len(lines):
            page["next"] = f"{end}:{digest}"
        return page

    def models(self, request: Ref, spawner: ProcessSpawner) -> Dict[str, Any]:
        asked = request.to_python()
        try:
            if not isinstance(asked, dict) or not set(asked) <= MODELS_KEYS:
                raise BadValue("the request has a key the echo does not know")
            self._applied(asked.get("account"), spawner)
        finally:
            spawner.destroy()
        return {"known": True, "models": [{"id": "echo"}]}

    def usage(self, query: Ref, spawner: ProcessSpawner) -> Dict[str, Any]:
        asked = query.to_python()
        try:
            if not isinstance(asked, dict) or not set(asked) <= QUERY_KEYS:
                raise BadValue("the query has a key the echo does not know")
            self._applied(asked.get("account"), spawner)
        finally:
            spawner.destroy()
        if not self.scripted:
            return {
                "supported": False,
                "unsupported_because": "this echo reports no usage",
                "sources": [],
                "account": None,
                "revision": 0,
                "observed_at": 0,
                "buckets": [],
                "limit": None,
                "spend": None,
            }
        account = asked["account"]["id"] if asked.get("account") is not None else None
        with self.reports.lock:
            held = self.reports.held.get(account or "")
        if held is not None:
            return dict(held)
        return {
            "supported": True,
            "unsupported_because": None,
            "sources": ["reported"],
            "account": account,
            "revision": 0,
            "observed_at": 0,
            "buckets": [],
            "limit": None,
            "spend": None,
        }
