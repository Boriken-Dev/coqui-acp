"""An `account_provider` library written in Python: the echo of `coqui_acp_api::contract::AccountContract`.

The module is the library: its id is its name, its version `__version__`, its providers the classes in
`__all__`. It keeps the accounts of an imaginary service, of two kinds. `echo-login` is a login kept in
`auth.json` of the account's directory, which rotates, renews with no model turn, is watched for a change
made behind the host's back, and reports plan usage; it provides the standard `echo-login`. `echo-key` is
a key the person pastes, which provides `echo-api` and reports no usage. Its login methods are scripted
(`scripted`, `waiting`, `api-key`); the setting `logins = false` offers none, `usage = false` reports none.
"""

from __future__ import annotations

import queue
import threading
import time
from dataclasses import dataclass
from typing import Any, Dict, List, Optional, Tuple

from coqui_acp_api_kinds import AccountKeeper, AccountProvider, AccountWatch, LoginPrompter, ProcessSpawner

from guatiao import Ref, provide
from guatiao.errors import BadValue, Gone, Unsupported

__version__ = "1.0.0"
__all__ = ["EchoAccounts"]

FILE_KIND = "echo-login"
KEY_KIND = "echo-key"
FILE = "auth.json"
FILE_KEY = "auth_file"
FILE_MAX = 1 << 20
ACCOUNT_ID_MAX = 128

#: The keys a request may have: one it does not declare is refused, never dropped.
LOGIN_KEYS = {"method", "inputs", "state_dir", "timeout_ms"}
RENEW_KEYS = {"account"}
WATCH_KEYS = {"account"}
USAGE_KEYS = {"account", "environment"}
GRANT_KEYS = {"id", "kind", "settings", "credentials", "state_dir", "revision", "provides"}


def _py(value: Any) -> Any:
    """A value a host object answered, as plain Python."""
    return value.to_python() if hasattr(value, "to_python") else value


def _credential() -> Dict[str, Any]:
    return {"type": "string", "writeOnly": True, "x-sensitive": True}


def _offer(logins: bool, usage: bool) -> Dict[str, Any]:
    """What the echo makes."""
    url = {"type": "string", "format": "uri"}
    endpoint = [{"key": "base_url", "purpose": "model_api", "default": "https://echo.example"}]
    kinds = [
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
            "provides": ["echo-login"],
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
    methods: List[Dict[str, Any]] = []
    if logins:
        methods = [
            {
                "id": "scripted",
                "title": "Scripted",
                "description": "",
                "produces": FILE_KIND,
                "inputs": {
                    "type": "object",
                    "properties": {"email": {"type": "string", "default": "person@example.invalid"}},
                },
                "steps": ["ask", "open_url", "user_code"],
                "remote": True,
            },
            {
                "id": "waiting",
                "title": "Waiting for a page",
                "description": "",
                "produces": FILE_KIND,
                "inputs": {},
                "steps": ["open_url"],
                "remote": False,
            },
            {
                "id": "api-key",
                "title": "A key",
                "description": "",
                "produces": KEY_KIND,
                "inputs": {},
                "steps": ["ask"],
                "remote": False,
            },
        ]
    return {"kinds": kinds, "logins": methods, "usage": [FILE_KIND] if usage else []}


def _is_account_id(text: Any) -> bool:
    return (
        isinstance(text, str)
        and 1 <= len(text.encode("utf-8")) <= ACCOUNT_ID_MAX
        and not any(ord(c) < 32 or 127 <= ord(c) < 160 for c in text)
    )


def _read_grant(offer: Dict[str, Any], grant: Any, spawner: ProcessSpawner) -> Dict[str, Any]:
    """Checks a grant of a kind the echo lists, and what it needs of `spawner`."""
    if not isinstance(grant, dict) or not set(grant) <= GRANT_KEYS:
        raise BadValue("the account has a key the echo does not know")
    if not _is_account_id(grant.get("id")):
        raise BadValue("an account id is 1 to 128 bytes with no control character")
    kind = grant.get("kind")
    if kind not in [k["id"] for k in offer["kinds"]]:
        raise BadValue("the echo makes no account of that kind")
    settings = grant.get("settings", {})
    if not isinstance(settings, dict):
        raise BadValue("the settings are not an object")
    for key, value in settings.items():
        url = isinstance(value, str) and (value.startswith("https://") or value.startswith("http://"))
        if key != "base_url" or not url:
            raise BadValue("the settings do not fit the kind's schema")
    credentials = grant.get("credentials", {})
    wanted = FILE_KEY if kind == FILE_KIND else "api_key"
    if not isinstance(credentials, dict) or sorted(credentials) != [wanted] or not credentials[wanted]:
        raise BadValue("a credential is missing, or one the kind does not take was given")
    if kind == FILE_KIND:
        if grant.get("state_dir") is None:
            raise BadValue("this kind needs a directory and none was granted")
        if not _py(spawner.describe()).get("files"):
            raise Unsupported("this spawner has no private files, which an echo login needs")
    return grant


def _key_of(grant: Dict[str, Any]) -> str:
    """What the echo keeps an account's file under: the same account in another directory is another
    file."""
    return f"{grant['id']}\0{grant.get('state_dir') or ''}"


class _Sub:
    """A keeper that is live for an account: its watch holds it, and lets go of the object (`release`)
    before `stop` answers."""

    def __init__(self, keeper: AccountKeeper) -> None:
        self.lock = threading.Lock()
        self.keeper: Optional[AccountKeeper] = keeper

    def release(self) -> None:
        with self.lock:
            keeper, self.keeper = self.keeper, None
        if keeper is not None:
            keeper.destroy()


class _Accounts:
    """The files of the accounts an instance watches, and who is listening for each."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        #: key -> {"last", "seen" ("present", "blank" or "gone"), "revision", "sequence"}
        self.held: Dict[str, Dict[str, Any]] = {}
        #: key -> the subs live for it
        self.subs: Dict[str, List["_Sub"]] = {}


def _tell(accounts: _Accounts, key: str, method: str, event: Dict[str, Any]) -> None:
    """Tells every keeper live for the account, not the first to ask."""
    with accounts.lock:
        live = list(accounts.subs.get(key, ()))
    for sub in live:
        with sub.lock:
            if sub.keeper is not None:
                try:
                    getattr(sub.keeper, method)(event)
                except Exception:  # a keeper that fails hears the next event
                    pass


def _read_text(spawner: ProcessSpawner, path: str) -> str:
    return bytes(_py(spawner.read_file(path, FILE_MAX))).decode("utf-8", "replace")


def _look(accounts: _Accounts, key: str, spawner: ProcessSpawner) -> None:
    """Looks at the account's file and tells every live keeper what happened to it behind the host's
    back: a changed file is renewed, a blank or deleted one lost. What is seen is recorded under the
    lock before anyone is told, so two watches never report one change twice."""
    ident = key.split("\0")[0]
    text = None if _py(spawner.file_info(FILE)) is None else _read_text(spawner, FILE)
    told: Optional[Tuple[str, Dict[str, Any]]] = None
    with accounts.lock:
        entry = accounts.held.get(key)
        if entry is None:
            return
        if text is None or not text.strip():
            seen = "gone" if text is None else "blank"
            if entry["seen"] != seen:
                entry["seen"] = seen
                entry["sequence"] += 1
                why = "the login file is gone" if text is None else "the login file is blank"
                told = ("lost", {"id": ident, "reason": why, "sequence": entry["sequence"]})
        else:
            entry["seen"] = "present"
            if text != entry["last"]:
                entry["last"] = text
                entry["sequence"] += 1
                told = ("renewed", {
                    "id": ident,
                    "from_revision": entry["revision"],
                    "credentials": {FILE_KEY: text},
                    "sequence": entry["sequence"],
                })
    if told is not None:
        _tell(accounts, key, told[0], told[1])


def _watch_loop(
    stopped: threading.Event, accounts: _Accounts, key: str, spawner: ProcessSpawner, sub: _Sub, poll: float
) -> None:
    """The watch's thread. It holds the event, not the `Watching` object, so dropping that object stops it."""
    try:
        while not stopped.is_set():
            try:
                _look(accounts, key, spawner)
            except Exception:  # the file is looked at again on the next tick
                pass
            stopped.wait(poll)
    finally:
        sub.release()
        spawner.destroy()


class Watching(AccountWatch):
    """A watch of one account: a thread of its own looks at the file, and stopping it ends the thread,
    which lets go of the spawner and the keeper before it ends."""

    def __init__(self, accounts: _Accounts, key: str, spawner: ProcessSpawner, sub: _Sub, poll: float) -> None:
        self._stopped = threading.Event()
        self._thread = threading.Thread(
            target=_watch_loop, args=(self._stopped, accounts, key, spawner, sub, poll), name="echo_watch", daemon=True
        )
        self._thread.start()

    def stop(self) -> None:
        self._stopped.set()
        if self._thread is not threading.current_thread():
            self._thread.join()

    def __del__(self) -> None:
        self.stop()


def _ended(cancelled: bool) -> Gone:
    return Gone("login cancelled" if cancelled else "login timed out")


class _Cli:
    """The notional CLI of a login, killed by whoever ends the login first."""

    def __init__(self, process: Any) -> None:
        self.lock = threading.Lock()
        self.process = process

    def kill(self) -> None:
        with self.lock:
            process, self.process = self.process, None
        if process is not None:
            try:
                process.kill()
            except Exception:
                pass
            process.destroy()


class _Signing:
    """A login in progress: its deadline, the prompter and the notional CLI whose tree a cancel kills."""

    def __init__(self, deadline: float, prompter: LoginPrompter, cli: _Cli) -> None:
        self.deadline = deadline
        self.prompter = prompter
        self.cli = cli

    def check(self) -> None:
        """Ends the login with GONE when the person cancelled or the time is up."""
        cancelled = bool(self.prompter.cancelled())
        if cancelled or time.monotonic() >= self.deadline:
            raise self.stop(cancelled)

    def stop(self, cancelled: bool) -> Gone:
        self.cli.kill()
        return _ended(cancelled)

    def prompt(self, call: Any) -> Any:
        """Runs one prompter call: GONE from it is the person cancelling."""
        try:
            answer = call(self.prompter)
        except Gone:
            raise self.stop(True) from None
        self.check()
        return answer


def _run_login(method: str, timeout_ms: int, signing: _Signing, spawner: ProcessSpawner) -> Dict[str, Any]:
    """The login itself, on a thread of its own so that the caller can answer at the deadline."""
    signing.check()

    def form(title: str, field: str) -> Dict[str, Any]:
        return {
            "title": title,
            "schema": {"type": "object", "properties": {field: _credential()}, "required": [field]},
        }

    if method == "scripted":
        signing.prompt(lambda p: p.say("Signing in as the echo does"))
        signing.prompt(lambda p: p.open_url(
            {"url": "https://echo.example/device", "user_code": "ECHO-0000", "expires_ms": timeout_ms}
        ))
        answer = _py(signing.prompt(lambda p: p.ask(form("Paste the code", "code"))))
        code = (answer.get("secrets") or {}).get("code", "")
        if not code or code == "refuse-me":
            raise BadValue("login refused: the echo does not take that code")
        spawner.write_file(FILE, ('{"token":"%s"}' % code).encode("utf-8"), True)
        outcome: Dict[str, Any] = {
            "kind": FILE_KIND,
            "settings": {},
            "credentials": {FILE_KEY: _read_text(spawner, FILE)},
            "label": "person@example.invalid",
            "expires_at": "2099-01-01T00:00:00Z",
            "checked": True,
        }
    elif method == "waiting":
        signing.prompt(lambda p: p.say("Waiting for the page"))
        signing.prompt(lambda p: p.open_url({"url": "https://echo.example/browser"}))
        # The person finishes in the browser; the CLI would then print.
        until = time.monotonic() + 0.4
        while time.monotonic() < until:
            signing.check()
            time.sleep(0.02)
        text = '{"token":"from-the-browser"}'
        spawner.write_file(FILE, text.encode("utf-8"), True)
        outcome = {"kind": FILE_KIND, "settings": {}, "credentials": {FILE_KEY: text}, "checked": True}
    else:
        answer = _py(signing.prompt(lambda p: p.ask(form("Paste the key", "api_key"))))
        key = (answer.get("secrets") or {}).get("api_key", "")
        if not key or key == "refuse-me":
            raise BadValue("login refused: the echo does not take that key")
        outcome = {"kind": KEY_KIND, "settings": {}, "credentials": {"api_key": key}, "checked": False}
    signing.check()
    return outcome


@dataclass(frozen=True)
class EchoConfig:
    """How an echo is configured."""

    #: Its name for people.
    title: str = "Echo accounts, in Python"
    #: Whether it lists login methods; when not, it lists none and refuses `login`.
    logins: bool = True
    #: Whether it reports plan usage for its login kind.
    usage: bool = True
    #: How often, in milliseconds, a watch looks at the account's file.
    poll_ms: int = 100


class EchoAccounts(provide.Provider, AccountProvider, display_name="Echo accounts, in Python", default=True):
    """Keeps the accounts of an imaginary service."""

    def __init__(self, config: EchoConfig = EchoConfig()) -> None:
        self.title = config.title
        self.logins = config.logins
        self.usage_on = config.usage
        self.poll = max(config.poll_ms, 1) / 1000.0
        self.accounts = _Accounts()
        self.revision = 0
        self.revision_lock = threading.Lock()

    def describe(self) -> Dict[str, Any]:
        return {"title": self.title, "description": "Keeps the accounts of an imaginary service."}

    def offers(self) -> Dict[str, Any]:
        return _offer(self.logins, self.usage_on)

    def login(self, request: Ref, spawner: ProcessSpawner, prompter: LoginPrompter) -> Dict[str, Any]:
        asked = request.to_python()
        started = False
        try:
            if not self.logins:
                raise Unsupported("this echo is configured to offer no login method")
            if not isinstance(asked, dict) or not set(asked) <= LOGIN_KEYS:
                raise BadValue("the login has a key the echo does not know")
            method = asked.get("method")
            if method not in [m["id"] for m in _offer(True, True)["logins"]]:
                raise BadValue("the echo has no such login method")
            timeout_ms = asked.get("timeout_ms", 600000)
            deadline = time.monotonic() + timeout_ms / 1000.0
            # The notional CLI, so that a cancel has a tree to kill.
            try:
                process = spawner.spawn({"program": "echo-login-cli", "cwd": asked["state_dir"]})
            except Exception:
                process = None
            cli = _Cli(process)
            signing = _Signing(deadline, prompter, cli)
            done: "queue.Queue[Tuple[bool, Any]]" = queue.Queue()

            def work() -> None:
                # The thread lets go of what it holds before it hands the caller the result, or when
                # it ends if the caller has already answered at the deadline.
                try:
                    result: Tuple[bool, Any] = (True, _run_login(method, timeout_ms, signing, spawner))
                except BaseException as e:
                    result = (False, e)
                spawner.destroy()
                prompter.destroy()
                done.put(result)

            # The login runs on a thread of its own, so that the answer comes at the deadline even
            # when a prompter call has not returned.
            threading.Thread(target=work, name="echo_login", daemon=True).start()
            started = True
            try:
                ok, result = done.get(timeout=max(deadline - time.monotonic(), 0))
            except queue.Empty:
                cli.kill()
                raise _ended(False) from None
            cli.kill()
            if not ok:
                raise result
            return result
        finally:
            if not started:
                spawner.destroy()
                prompter.destroy()

    def renew(self, request: Ref, spawner: ProcessSpawner) -> Dict[str, Any]:
        asked = request.to_python()
        try:
            if not isinstance(asked, dict) or not set(asked) <= RENEW_KEYS:
                raise BadValue("the request has a key the echo does not know")
            offer = _offer(self.logins, self.usage_on)
            grant = _read_grant(offer, asked.get("account"), spawner)
            kind = next(k for k in offer["kinds"] if k["id"] == grant["kind"])
            if not kind["renews"]:
                raise Unsupported("this kind cannot be renewed without a model turn")
            # A login that is blank or gone is lost, whatever the grant says: nothing is renewed from it.
            key = _key_of(grant)
            with self.accounts.lock:
                self.accounts.held.setdefault(
                    key, {"last": "", "seen": "present", "revision": grant.get("revision", 0), "sequence": 0}
                )
            if _py(spawner.file_info(FILE)) is None:
                _look(self.accounts, key, spawner)
                raise Gone("the login file is gone")
            text = _read_text(spawner, FILE)
            if not text.strip():
                _look(self.accounts, key, spawner)
                raise Gone("the login file is blank")
            text += "#renewed"
            spawner.write_file(FILE, text.encode("utf-8"), True)
            with self.accounts.lock:
                entry = self.accounts.held[key]
                entry["last"] = text
                entry["seen"] = "present"
            return {"credentials": {FILE_KEY: text}, "expires_at": "2099-01-01T00:00:00Z"}
        finally:
            spawner.destroy()

    def watch(self, request: Ref, spawner: ProcessSpawner, keeper: AccountKeeper) -> AccountWatch:
        sub = _Sub(keeper)
        try:
            asked = request.to_python()
            if not isinstance(asked, dict) or not set(asked) <= WATCH_KEYS:
                raise BadValue("the request has a key the echo does not know")
            offer = _offer(self.logins, self.usage_on)
            grant = _read_grant(offer, asked.get("account"), spawner)
            kind = next(k for k in offer["kinds"] if k["id"] == grant["kind"])
            if not (kind["rotates"] and kind["state_dir"]):
                raise Unsupported("this kind keeps no login to watch")
            key = _key_of(grant)
            with self.accounts.lock:
                entry = self.accounts.held.setdefault(
                    key,
                    {
                        "last": grant["credentials"][FILE_KEY],
                        "seen": "present",
                        "revision": grant.get("revision", 0),
                        "sequence": 0,
                    },
                )
                entry["revision"] = grant.get("revision", 0)
                self.accounts.subs.setdefault(key, []).append(sub)
            return Watching(self.accounts, key, spawner, sub, self.poll)
        except BaseException:
            sub.release()
            spawner.destroy()
            raise

    def usage(self, query: Ref, spawner: ProcessSpawner, keeper: AccountKeeper) -> Dict[str, Any]:
        keeper.destroy()
        try:
            asked = query.to_python()
            if not isinstance(asked, dict) or not set(asked) <= USAGE_KEYS:
                raise BadValue("the query has a key the echo does not know")
            offer = _offer(self.logins, self.usage_on)
            grant = _read_grant(offer, asked.get("account"), spawner)
        finally:
            spawner.destroy()
        if grant["kind"] not in offer["usage"]:
            return {
                "supported": False,
                "unsupported_because": "the echo reports no usage for this kind",
                "sources": [],
                "account": grant["id"],
                "revision": 0,
                "observed_at": 0,
                "buckets": [],
            }
        at = int(time.time())
        buckets = [
            _bucket(
                "five_hour",
                label="Five hours",
                models={"kind": "all"},
                used=420,
                limit=1000,
                refill={"kind": "fixed_window", "length": [5 * 3600, 0], "resets_at": at + 3600},
                status="ok",
                observed_at=at,
            ),
            _bucket(
                "seven_day_large",
                label="Seven days, the large model",
                models={"kind": "only", "models": ["echo-large"]},
                used=1000,
                limit=1000,
                refill={"kind": "fixed_window", "length": [7 * 24 * 3600, 0], "resets_at": at + 86400},
                status="exhausted",
                observed_at=at,
            ),
        ]
        with self.revision_lock:
            self.revision += 1
            revision = self.revision
        return {
            "supported": True,
            "unsupported_because": None,
            "sources": ["reported"],
            "account": grant["id"],
            "revision": revision,
            "observed_at": at,
            "buckets": buckets,
        }


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
