#!/usr/bin/env python3
"""Record pi's RPC / JSON / print modes against the loopback stand-in.

Usage: record_pi.py SCRATCH PI_CLI_JS OUTDIR [scenario ...]

SCRATCH   a throwaway directory (made if missing); the agent dir, HOME, work dirs
          and the stand-in's request log live under it
PI_CLI_JS pi's entry, <prefix>/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js
OUTDIR    where the transcripts are written, one text file per scenario

`node` must be on PATH. Each scenario gets a fresh agent dir (a copy of the template
written by `make_template`) and a fresh work dir, so the file listing at the end of
a transcript is what that scenario alone wrote.

Transcript line prefixes: `> ` a line sent to pi's stdin, `< ` a line read from its
stdout, `! ` a line from its stderr, `# ` a note by this recorder (never pi output).
Values are replaced by placeholders: <SCRATCH>, <HOME>, <AGENTDIR>, <WORK>, <PORT>.
Ids and timestamps are left as pi wrote them.
"""
import base64
import json
import os
import queue
import re
import shutil
import socket
import subprocess
import sys
import threading
import time

SCRATCH = os.path.abspath(sys.argv[1])
CLI = os.path.abspath(sys.argv[2])
OUT = os.path.abspath(sys.argv[3])
ONLY = sys.argv[4:]
HERE = os.path.dirname(os.path.abspath(__file__))

PNG_1X1 = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)
CRED_WORDS = re.compile(r"(KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|^AWS_|^AZURE_|^GOOGLE_|^OPENAI|^ANTHROPIC)", re.I)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


PORT = free_port()
REQLOG = os.path.join(SCRATCH, "requests.jsonl")
SERVER = None


def start_server():
    global SERVER
    SERVER = subprocess.Popen(
        [sys.executable, os.path.join(HERE, "openai_chat_model.py"), str(PORT), REQLOG],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    SERVER.stdout.readline()


def make_template(port):
    t = os.path.join(SCRATCH, "template")
    shutil.rmtree(t, ignore_errors=True)
    os.makedirs(t)
    models = {
        "providers": {
            "stub": {
                "baseUrl": "http://127.0.0.1:%d/v1" % port,
                "api": "openai-completions",
                "apiKey": "sk-test",
                "compat": {"supportsReasoningEffort": True, "thinkingFormat": "openai"},
                "models": [
                    {"id": "stub-fast", "name": "Stub Fast", "input": ["text"], "contextWindow": 128000, "maxTokens": 4096},
                    {
                        "id": "stub-think",
                        "name": "Stub Think",
                        "reasoning": True,
                        "input": ["text", "image"],
                        "contextWindow": 32000,
                        "maxTokens": 4096,
                        "cost": {"input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 1.25},
                    },
                ],
            }
        }
    }
    with open(os.path.join(t, "models.json"), "w", newline="\n") as f:
        json.dump(models, f, indent=2)
        f.write("\n")
    settings = {"defaultProvider": "stub", "defaultModel": "stub-think", "defaultThinkingLevel": "medium"}
    with open(os.path.join(t, "settings.json"), "w", newline="\n") as f:
        json.dump(settings, f, indent=2)
        f.write("\n")
    return t


def scrub(text, ctx):
    """Replace machine-specific values with placeholders (raw, JSON-escaped, forward-slash)."""
    pairs = [
        (ctx["agentdir"], "<AGENTDIR>"),
        (ctx["work"], "<WORK>"),
        (ctx["home"], "<HOME>"),
        (SCRATCH, "<SCRATCH>"),
        (HERE, "<RECORDER>"),  # the directory holding this script and the .ts extensions
    ]
    for real, ph in pairs:
        for form in {real, real.replace("\\", "/"), real.replace("\\", "\\\\"), json.dumps(real)[1:-1]}:
            text = text.replace(form, ph)
    text = text.replace("127.0.0.1:%d" % PORT, "127.0.0.1:<PORT>")
    # the session directory name is the work dir with separators and the drive colon turned into "-"
    text = text.replace(re.sub(r"[\\/:]", "-", ctx["work"]), "<WORK-ENCODED>")
    if ctx.get("deadport"):
        text = text.replace("127.0.0.1:%d" % ctx["deadport"], "127.0.0.1:<DEADPORT>").replace("/127.0.0.1:%d/" % ctx["deadport"], "/127.0.0.1:<DEADPORT>/")
    # separators after a placeholder: show "/" only. In JSON text one backslash is written \\, so
    # only that pair is converted (a single backslash there starts an escape such as \n).
    seg = r"(?:\\\\|/)(?:[^\\\"\s<]|<WORK-ENCODED>)+"
    text = re.sub(r"<(?:AGENTDIR|WORK|HOME|SCRATCH|RECORDER)>(?:%s)+" % seg, lambda m: m.group(0).replace("\\\\", "/"), text)
    out = []
    for line in text.split("\n"):
        if line.startswith(("! ", "# ")):  # not JSON: a plain backslash is a separator
            line = re.sub(r"<(?:AGENTDIR|WORK|HOME|SCRATCH|RECORDER)>(?:[\\/](?:[^\s<\"']|<WORK-ENCODED>)+)+", lambda m: m.group(0).replace("\\", "/"), line)
        out.append(line)
    return "\n".join(out)


class Run:
    """One pi process with a transcript."""

    def __init__(self, name, ctx, args, extra_env=None, note=None, close_stdin=False):
        self.ctx = ctx
        self.lines = ctx["transcript"]
        self.q = queue.Queue()
        env = {k: v for k, v in os.environ.items() if not CRED_WORDS.search(k)}
        env.update(
            HOME=ctx["home"],
            USERPROFILE=ctx["home"],
            APPDATA=os.path.join(ctx["home"], "AppData"),
            LOCALAPPDATA=os.path.join(ctx["home"], "Local"),
            TEMP=os.path.join(ctx["home"], "tmp"),
            TMP=os.path.join(ctx["home"], "tmp"),
            PI_CODING_AGENT_DIR=ctx["agentdir"],
            PI_SKIP_VERSION_CHECK="1",
            PI_OFFLINE="1",
            NO_COLOR="1",
        )
        env.update(extra_env or {})
        cmd = ["node", CLI] + args
        self.note("start: pi " + " ".join(args))
        if extra_env:
            self.note("env added: " + ", ".join(sorted(extra_env)))
        self.p = subprocess.Popen(
            cmd, cwd=ctx["work"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
        if close_stdin:
            # print and json modes read piped stdin to its end and prepend it to the prompt
            self.note("stdin closed at once (a piped stdin is read to EOF before the prompt runs)")
            self.p.stdin.close()
        self.events = []
        self.n_settled = 0  # agent_settled records read while waiting for something else
        self.n_taken = 0
        self.out_done = threading.Event()
        threading.Thread(target=self._read, args=(self.p.stdout, "<"), daemon=True).start()
        threading.Thread(target=self._read, args=(self.p.stderr, "!"), daemon=True).start()

    def note(self, text):
        self.lines.append("# " + text)

    def _read(self, stream, mark):
        buf = b""
        while True:
            chunk = stream.read1(65536) if hasattr(stream, "read1") else stream.read(1)
            if not chunk:
                break
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                text = line.decode("utf-8", "replace").rstrip("\r")
                self.lines.append("%s %s" % (mark, text))
                if mark == "<":
                    try:
                        self.q.put(json.loads(text))
                    except Exception:
                        self.q.put({"_raw": text})
        if buf:
            self.lines.append("%s %s" % (mark, buf.decode("utf-8", "replace")))
        if mark == "<":
            self.q.put({"_eof": True})

    def send(self, obj):
        line = obj if isinstance(obj, str) else json.dumps(obj, separators=(",", ":"))
        self.lines.append("> " + line.replace("\r", "<CR>"))  # transcripts stay LF-only
        try:
            self.p.stdin.write(line.encode("utf-8") + b"\n")
            self.p.stdin.flush()
        except OSError:
            self.note("stdin write failed (process gone)")

    def wait(self, pred, timeout=30.0, what=""):
        """Read stdout records until pred(record) is true; returns it or None on timeout/EOF."""
        end = time.time() + timeout
        while time.time() < end:
            try:
                rec = self.q.get(timeout=max(0.05, end - time.time()))
            except queue.Empty:
                continue
            if rec.get("_eof"):
                self.note("stdout closed while waiting for %s" % (what or "a record"))
                return None
            self.events.append(rec)
            if rec.get("type") == "agent_settled":
                self.n_settled += 1
            if pred(rec):
                return rec
        self.note("timeout (%ss) waiting for %s" % (timeout, what or "a record"))
        return None

    def response(self, cmd_id, timeout=30.0):
        return self.wait(lambda r: r.get("type") == "response" and r.get("id") == cmd_id, timeout, "response " + cmd_id)

    def call(self, cmd_id, type_, timeout=30.0, **kw):
        obj = {"id": cmd_id, "type": type_}
        obj.update(kw)
        self.send(obj)
        return self.response(cmd_id, timeout)

    def settled(self, timeout=60.0):
        """Wait for the next agent_settled not yet taken (one may already have been read by call())."""
        if self.n_settled > self.n_taken:
            self.n_taken += 1
            return True
        rec = self.wait(lambda r: r.get("type") == "agent_settled", timeout, "agent_settled")
        if rec:
            self.n_taken += 1
        return rec

    def drain(self, seconds=1.0):
        end = time.time() + seconds
        while time.time() < end:
            try:
                rec = self.q.get(timeout=0.1)
            except queue.Empty:
                continue
            if rec.get("_eof"):
                return
            self.events.append(rec)

    def close(self, timeout=15.0):
        self.note("close stdin")
        try:
            self.p.stdin.close()
        except OSError:
            pass
        t0 = time.time()
        try:
            rc = self.p.wait(timeout)
            self.note("exit code %s after %.1fs" % (rc, time.time() - t0))
        except subprocess.TimeoutExpired:
            self.note("still running after %ss; killed" % timeout)
            self.p.kill()
            self.p.wait()
        self.drain(0.3)

    def kill(self):
        if self.p.poll() is None:
            self.p.kill()
            self.p.wait()
        self.drain(0.3)

    def exit_code(self, timeout=30.0):
        try:
            rc = self.p.wait(timeout)
        except subprocess.TimeoutExpired:
            self.note("still running after %ss; killed" % timeout)
            self.p.kill()
            self.p.wait()
            rc = None
        time.sleep(0.3)
        self.note("exit code %s" % rc)
        return rc


# ------------------------------------------------------------------ scenarios


def rpc(ctx, extra=None, model=None):
    args = ["--mode", "rpc"]
    args += extra or []
    return Run("rpc", ctx, args)


def sc_startup_state(ctx):
    r = rpc(ctx)
    r.note("nothing is expected on stdout before the first command; waiting 3 s")
    r.drain(3.0)
    r.call("1", "get_state")
    r.call("2", "get_available_models")
    r.call("3", "get_available_thinking_levels")
    r.call("4", "get_commands")
    r.call("5", "get_session_stats")
    r.call("6", "get_messages")
    r.close()


def sc_models_thinking(ctx):
    r = rpc(ctx)
    r.call("1", "get_state")
    r.call("2", "set_model", provider="stub", modelId="stub-fast")
    r.call("3", "get_available_thinking_levels")
    r.call("4", "set_thinking_level", level="high")
    r.call("5", "get_state")
    r.call("6", "set_model", provider="stub", modelId="stub-think")
    r.call("7", "get_available_thinking_levels")
    r.call("8", "set_thinking_level", level="high")
    r.call("9", "set_thinking_level", level="xhigh")
    r.call("10", "get_state")
    r.call("11", "cycle_thinking_level")
    r.call("12", "cycle_model")
    r.call("13", "set_model", provider="stub", modelId="no-such-model")
    r.call("14", "set_thinking_level", level="bogus")
    r.call("15", "set_thinking_level", level="off")
    r.call("16", "prompt", message="hello after switches")
    r.settled()
    r.close()
    r.note("provider request bodies for this scenario are in requests.jsonl")


def sc_prompt_text(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="Say hello")
    r.settled()
    r.call("2", "get_messages")
    r.call("3", "get_last_assistant_text")
    r.call("4", "get_session_stats")
    r.call("5", "get_state")
    r.close()


def sc_prompt_thinking(ctx):
    r = rpc(ctx)
    r.call("1", "set_thinking_level", level="high")
    r.call("2", "prompt", message="[[think]] what is the answer")
    r.settled()
    r.call("3", "get_messages")
    r.close()


def sc_prompt_image(ctx):
    r = rpc(ctx)
    img = {"type": "image", "data": base64.b64encode(PNG_1X1).decode(), "mimeType": "image/png"}
    r.call("1", "prompt", message="What is in this image?", images=[img])
    r.settled()
    r.call("2", "get_messages")
    r.note("image to a text-only model (stub-fast):")
    r.call("3", "set_model", provider="stub", modelId="stub-fast")
    r.call("4", "prompt", message="And this one?", images=[img])
    r.settled()
    r.close()


def sc_tool_read(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="[[read:notes.txt]] show me the file")
    r.settled()
    r.call("2", "get_messages")
    r.note("a path that does not exist:")
    r.call("3", "prompt", message="[[read:missing.txt]] show me the file")
    r.settled()
    r.close()


def sc_tool_write(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="[[write:out/new.txt]] create it")
    r.settled()
    r.note("file on disk afterwards: " + repr(open(os.path.join(ctx["work"], "out", "new.txt"), encoding="utf-8", newline="").read()) if os.path.exists(os.path.join(ctx["work"], "out", "new.txt")) else "file on disk afterwards: missing")
    r.close()


def sc_tool_edit(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="[[edit:notes.txt]] change alpha")
    r.settled()
    r.note("file on disk afterwards: " + repr(open(os.path.join(ctx["work"], "notes.txt"), encoding="utf-8", newline="").read()))
    r.note("edit of text that is not in the file (second run, alpha is gone):")
    r.call("2", "prompt", message="[[edit:notes.txt]] change alpha again")
    r.settled()
    r.close()


def sc_tool_bash(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="[[bash:echo hi from bash && echo to-stderr 1>&2 && exit 3]] run it")
    r.settled()
    r.call("2", "get_messages")
    r.close()


def sc_tool_two(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="[[two:notes.txt]] two calls")
    r.settled()
    r.close()


def sc_rpc_bash(ctx):
    r = rpc(ctx)
    r.call("1", "bash", command="echo from rpc bash; echo err 1>&2")
    r.call("2", "bash", command="echo hidden", excludeFromContext=True)
    r.call("3", "get_messages")
    r.send({"id": "4", "type": "bash", "command": "sleep 30"})
    time.sleep(1.5)
    r.call("5", "abort_bash")
    r.response("4")
    r.close()


def sc_abort_turn(ctx):
    r = rpc(ctx)
    r.send({"id": "1", "type": "prompt", "message": "[[slow]] go slowly"})
    r.response("1")
    r.wait(lambda e: e.get("type") == "message_update" and "tick2 " in json.dumps(e), 15, "third tick")
    r.call("2", "abort")
    r.settled()
    r.call("3", "get_messages")
    r.call("4", "prompt", message="after the abort")
    r.settled()
    r.close()


def sc_abort_tool(ctx):
    r = rpc(ctx)
    r.send({"id": "1", "type": "prompt", "message": "[[bash:sleep 30]] wait"})
    r.response("1")
    r.wait(lambda e: e.get("type") == "tool_execution_start", 15, "tool start")
    time.sleep(1.0)
    r.call("2", "abort")
    r.settled()
    r.call("3", "get_messages")
    r.close()


def sc_queue(ctx):
    r = rpc(ctx)
    r.send({"id": "1", "type": "prompt", "message": "[[slow]] first"})
    r.response("1")
    r.wait(lambda e: e.get("type") == "message_update", 15, "first update")
    r.call("2", "prompt", message="second without streamingBehavior")
    r.call("3", "prompt", message="second as steer", streamingBehavior="steer")
    r.call("4", "prompt", message="third as followUp", streamingBehavior="followUp")
    r.call("5", "clear_queue")
    r.call("6", "abort")
    r.settled()
    r.close()


def sc_session(ctx):
    sd = os.path.join(ctx["home"], "sessions")
    r = rpc(ctx, ["--session-dir", sd])
    r.call("1", "get_state")
    r.call("2", "prompt", message="first session says hi")
    r.settled()
    r.call("3", "get_state")
    r.call("4", "set_session_name", name="my first session")
    r.call("5", "new_session")
    r.call("6", "get_state")
    r.call("7", "prompt", message="second session says hi")
    r.settled()
    r.call("8", "get_entries")
    r.call("9", "get_tree")
    r.call("10", "get_fork_messages")
    first = next((e for e in r.events if e.get("id") == "3"), None)
    path = first["data"]["sessionFile"] if first and first.get("success") else None
    if path:
        r.call("11", "switch_session", sessionPath=path)
        r.call("12", "get_messages")
        r.call("13", "get_state")
    r.call("14", "switch_session", sessionPath=os.path.join(sd, "does-not-exist.jsonl"))
    r.close()
    r.note("files in the session dir:")
    for root, _d, files in os.walk(sd):
        for f in sorted(files):
            r.note("  " + os.path.relpath(os.path.join(root, f), sd).replace("\\", "/") + " %d bytes" % os.path.getsize(os.path.join(root, f)))
    if path and os.path.exists(path):
        r.note("session file contents:")
        for ln in open(path, encoding="utf-8", newline="").read().split("\n"):
            if ln:
                r.lines.append("@ " + ln)
    # resume in a new process
    r2 = rpc(ctx, ["--session-dir", sd, "--continue"])
    r2.note("second process: --continue")
    r2.call("1", "get_state")
    r2.call("2", "get_messages")
    r2.call("3", "prompt", message="after continue")
    r2.settled()
    r2.close()
    if path:
        sid = os.path.basename(path).split("_", 1)[-1].replace(".jsonl", "")
        r3 = rpc(ctx, ["--session-dir", sd, "--session", sid])
        r3.note("third process: --session <id>")
        r3.call("1", "get_state")
        r3.call("2", "get_messages")
        r3.close()
    r4 = rpc(ctx, ["--session-dir", sd, "--session", "no-such-session-id"])
    r4.note("fourth process: --session with an id that does not exist")
    r4.call("1", "get_state")
    r4.close()


def sc_fork_clone(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="one")
    r.settled()
    r.call("2", "prompt", message="two")
    r.settled()
    r.call("3", "get_fork_messages")
    ev = next((e for e in r.events if e.get("id") == "3" and e.get("success")), None)
    if ev and ev["data"]["messages"]:
        r.call("4", "fork", entryId=ev["data"]["messages"][-1]["entryId"])
        r.call("5", "get_messages")
    r.call("6", "clone")
    r.call("7", "get_state")
    r.call("8", "export_html", outputPath=os.path.join(ctx["work"], "export.html"))
    r.close()


def sc_compaction(ctx):
    # keep almost nothing so that a short session has something to summarize
    p = os.path.join(ctx["agentdir"], "settings.json")
    st = json.load(open(p))
    st["compaction"] = {"keepRecentTokens": 40}
    json.dump(st, open(p, "w"), indent=2)
    r = rpc(ctx)
    r.note('settings.json gained {"compaction":{"keepRecentTokens":40}}')
    for i in range(3):
        r.call("p%d" % i, "prompt", message="filler turn %d: " % i + "words " * 60)
        r.settled()
    r.call("c1", "compact", timeout=60)
    r.call("c2", "get_messages")
    r.call("c3", "get_session_stats")
    r.note("auto compaction: a reply that reports 60000 prompt tokens against a 32000 window")
    r.call("a1", "prompt", message="[[huge]] fill the window")
    r.settled(60)
    r.drain(2)
    r.call("a2", "prompt", message="next turn after the big one")
    r.settled(60)
    r.close()


def sc_auto_compaction(ctx):
    p = os.path.join(ctx["agentdir"], "settings.json")
    st = json.load(open(p))
    st["compaction"] = {"keepRecentTokens": 40}
    json.dump(st, open(p, "w"), indent=2)
    r = rpc(ctx)
    r.note('settings.json gained {"compaction":{"keepRecentTokens":40}}; window 32000, reserve 16384 (defaults)')
    for i in range(3):
        r.call("p%d" % i, "prompt", message="filler turn %d: " % i + "words " * 60)
        r.settled()
    r.note("a reply that reports 60000 prompt tokens")
    r.call("a1", "prompt", message="[[huge]] fill the window")
    r.settled(60)
    r.drain(3)
    r.note("the next prompt")
    r.call("a2", "prompt", message="next turn after the big one")
    r.settled(60)
    r.drain(3)
    r.call("a3", "get_session_stats")
    r.close()


def sc_errors(ctx):
    r = rpc(ctx)
    r.note("auto retry off: a 429, a 500 and a cut stream each end the run with an error message")
    r.call("0", "set_auto_retry", enabled=False)
    r.call("1", "prompt", message="[[429]] rate limited")
    r.settled()
    r.call("2", "get_messages")
    r.call("3", "prompt", message="[[500]] server error")
    r.settled()
    r.call("4", "prompt", message="[[drop]] stream cut")
    r.settled(60)
    r.note("auto retry on: 429 once, then fine")
    r.call("5", "set_auto_retry", enabled=True)
    r.call("6", "prompt", message="[[429once]] retry me")
    r.settled(60)
    r.call("7", "get_messages")
    r.close()


def sc_retry_abort(ctx):
    r = rpc(ctx)
    r.call("1", "set_auto_retry", enabled=True)
    r.call("2", "prompt", message="[[500]] retry then abort_retry")
    r.wait(lambda e: e.get("type") == "auto_retry_start", 30, "auto_retry_start")
    r.call("3", "abort_retry")
    r.settled(60)
    r.close()


def sc_unreachable(ctx):
    # the provider's baseUrl points at a port nothing listens on
    p = os.path.join(ctx["agentdir"], "models.json")
    m = json.load(open(p))
    dead = free_port()
    ctx["deadport"] = dead
    m["providers"]["stub"]["baseUrl"] = "http://127.0.0.1:%d/v1" % dead
    json.dump(m, open(p, "w"), indent=2)
    r = rpc(ctx)
    r.call("0", "set_auto_retry", enabled=False)
    r.call("1", "prompt", message="anyone there?")
    r.settled(60)
    r.call("2", "get_messages")
    r.close()


def sc_no_credentials(ctx):
    # an agent dir with no models.json, settings or key: what pi says
    for f in ("models.json", "settings.json"):
        os.remove(os.path.join(ctx["agentdir"], f))
    r = rpc(ctx)
    r.call("1", "get_state")
    r.call("2", "get_available_models")
    r.call("3", "prompt", message="hello")
    r.settled(20)
    r.close()
    pr = Run("print", ctx, ["-p", "hello"], close_stdin=True)
    pr.exit_code(30)
    js = Run("json", ctx, ["--mode", "json", "hello"], close_stdin=True)
    js.exit_code(30)


def sc_protocol_errors(ctx):
    r = rpc(ctx)
    r.send("this is not json")
    r.drain(1)
    r.send({"id": "1", "type": "no_such_command"})
    r.drain(1)
    r.send({"id": "2", "type": "prompt"})
    r.drain(1)
    r.send({"id": "3", "type": "set_model", "provider": "stub"})
    r.drain(1)
    r.send({"type": "get_state"})
    r.drain(1)
    r.send('{"id":"4","type":"get_state"}\r')
    r.drain(1)
    r.close()


def sc_mode_json(ctx):
    r = Run("json", ctx, ["--mode", "json", "--no-session", "Say hello"], close_stdin=True)
    r.exit_code(60)
    r2 = Run("json2", ctx, ["--mode", "json", "--no-session", "[[read:notes.txt]] read it"], close_stdin=True)
    r2.exit_code(60)
    r3 = Run("json3", ctx, ["--mode", "json", "--no-session", "[[500]] fail"], close_stdin=True)
    r3.exit_code(60)


def sc_mode_print(ctx):
    r = Run("print", ctx, ["-p", "--no-session", "Say hello"], close_stdin=True)
    r.exit_code(60)
    r2 = Run("print2", ctx, ["-p", "--no-session", "[[read:notes.txt]] read it"], close_stdin=True)
    r2.exit_code(60)
    r3 = Run("print3", ctx, ["-p", "--no-session", "[[500]] fail"], close_stdin=True)
    r3.exit_code(60)
    r4 = Run("print4", ctx, ["-p", "--no-session", "--mode", "text", "Say hello"], close_stdin=True)
    r4.exit_code(60)


def sc_versions(ctx):
    for a in (["--version"], ["-v"], ["--list-models"], ["--list-models", "stub"]):
        r = Run("v", ctx, a, close_stdin=True)
        r.exit_code(30)


def sc_startup_flags(ctx):
    r = rpc(ctx, ["--provider", "stub", "--model", "stub-fast", "--thinking", "high", "--no-session", "--name", "named", "--tools", "read"])
    r.note("--provider stub --model stub-fast --thinking high --no-session --name named --tools read")
    r.call("1", "get_state")
    r.call("2", "prompt", message="hello")
    r.settled()
    r.close()
    r2 = rpc(ctx, ["--model", "stub/stub-think:low", "--no-session"])
    r2.note("--model stub/stub-think:low (provider prefix and thinking suffix)")
    r2.call("1", "get_state")
    r2.close()
    r3 = rpc(ctx, ["--model", "nonexistent-model", "--no-session"])
    r3.note("--model that matches nothing")
    r3.call("1", "get_state")
    r3.close()
    r4 = rpc(ctx, ["--no-session", "@notes.txt"])
    r4.note("an @file argument is rejected in rpc mode")
    r4.call("1", "get_state")
    r4.close()


def sc_shutdown(ctx):
    r = rpc(ctx)
    r.send({"id": "1", "type": "prompt", "message": "[[slow]] long answer"})
    r.response("1")
    r.wait(lambda e: e.get("type") == "message_update", 15, "first update")
    r.note("stdin closed while the turn is running; the slow stand-in would stream for 30 s")
    t0 = time.time()
    r.close(timeout=45)
    r.note("elapsed %.1f s" % (time.time() - t0))


def sc_permission_gate(ctx):
    ext = os.path.join(HERE, "ask_every_tool.ts")
    r = rpc(ctx, ["-e", ext])
    r.note("extension %s asks before bash, write and edit" % os.path.basename(ext))
    r.send({"id": "1", "type": "prompt", "message": "[[bash:echo allowed]] run"})
    req = r.wait(lambda e: e.get("type") == "extension_ui_request" and e.get("method") in ("select", "confirm"), 30, "permission request")
    if req:
        r.send({"type": "extension_ui_response", "id": req["id"], "value": "Allow once"})
    r.settled()
    r.note("refused:")
    r.send({"id": "2", "type": "prompt", "message": "[[write:refused.txt]] write"})
    req = r.wait(lambda e: e.get("type") == "extension_ui_request" and e.get("method") in ("select", "confirm"), 30, "permission request")
    if req:
        r.send({"type": "extension_ui_response", "id": req["id"], "value": "Reject"})
    r.settled()
    r.note("dismissed with cancelled:true:")
    r.send({"id": "3", "type": "prompt", "message": "[[edit:notes.txt]] edit"})
    req = r.wait(lambda e: e.get("type") == "extension_ui_request" and e.get("method") in ("select", "confirm"), 30, "permission request")
    if req:
        r.send({"type": "extension_ui_response", "id": req["id"], "cancelled": True})
    r.settled()
    r.note("cancel (abort) while the question is open:")
    r.send({"id": "4", "type": "prompt", "message": "[[bash:echo never]] run"})
    req = r.wait(lambda e: e.get("type") == "extension_ui_request" and e.get("method") in ("select", "confirm"), 30, "permission request")
    r.call("5", "abort")
    r.settled(20)
    r.note("a late answer to the closed request:")
    if req:
        r.send({"type": "extension_ui_response", "id": req["id"], "value": "Allow once"})
    r.drain(2)
    r.close()
    r.note("the stand-in's own file on disk after the refused write: " + ("present" if os.path.exists(os.path.join(ctx["work"], "refused.txt")) else "absent"))


def sc_permission_gate_stock(ctx):
    ext = os.path.join(SCRATCH, "install", "node_modules", "@earendil-works", "pi-coding-agent", "examples", "extensions", "permission-gate.ts")
    r = rpc(ctx, ["-e", ext])
    r.note("the shipped example permission-gate.ts with a dangerous command, answered Yes")
    r.send({"id": "1", "type": "prompt", "message": "[[bash:rm -rf nothing_here]] run"})
    req = r.wait(lambda e: e.get("type") == "extension_ui_request" and e.get("method") in ("select", "confirm"), 30, "permission request")
    if req:
        r.send({"type": "extension_ui_response", "id": req["id"], "value": "Yes"})
    r.settled()
    r.close()


def sc_ui_methods(ctx):
    ext = os.path.join(HERE, "ui_probe.ts")
    r = rpc(ctx, ["-e", ext])
    r.note("extension %s calls every ctx.ui method from a /uiprobe command" % os.path.basename(ext))
    r.call("1", "get_commands")
    r.send({"id": "2", "type": "prompt", "message": "/uiprobe"})
    for _ in range(4):
        req = r.wait(lambda e: e.get("type") == "extension_ui_request" and e.get("method") in ("select", "confirm", "input", "editor"), 10, "dialog")
        if not req:
            break
        if req["method"] == "confirm":
            r.send({"type": "extension_ui_response", "id": req["id"], "confirmed": True})
        else:
            r.send({"type": "extension_ui_response", "id": req["id"], "value": "answer"})
    r.drain(2)
    r.close()


def sc_trust_and_context(ctx):
    pd = os.path.join(ctx["work"], ".pi")
    os.makedirs(os.path.join(pd, "extensions"), exist_ok=True)
    with open(os.path.join(pd, "settings.json"), "w", newline="\n") as f:
        f.write('{"defaultThinkingLevel":"low"}\n')
    with open(os.path.join(ctx["work"], "AGENTS.md"), "w", newline="\n") as f:
        f.write("Project rule: answer in one word.\n")
    r = rpc(ctx)
    r.note("project .pi/settings.json present and not trusted; AGENTS.md present")
    r.call("1", "get_state")
    r.call("2", "prompt", message="hi")
    r.settled()
    r.close()
    r2 = rpc(ctx, ["--approve"])
    r2.note("same with --approve")
    r2.call("1", "get_state")
    r2.close()


def sc_images_tool(ctx):
    r = rpc(ctx)
    r.call("1", "prompt", message="[[read:pic.png]] look at the picture")
    r.settled()
    r.call("2", "get_messages")
    r.close()


SCENARIOS = {
    "startup_state": sc_startup_state,
    "models_thinking": sc_models_thinking,
    "prompt_text": sc_prompt_text,
    "prompt_thinking": sc_prompt_thinking,
    "prompt_image": sc_prompt_image,
    "tool_read": sc_tool_read,
    "tool_read_image": sc_images_tool,
    "tool_write": sc_tool_write,
    "tool_edit": sc_tool_edit,
    "tool_bash": sc_tool_bash,
    "tool_two": sc_tool_two,
    "rpc_bash": sc_rpc_bash,
    "abort_turn": sc_abort_turn,
    "abort_tool": sc_abort_tool,
    "queue": sc_queue,
    "session": sc_session,
    "fork_clone": sc_fork_clone,
    "compaction": sc_compaction,
    "auto_compaction": sc_auto_compaction,
    "errors": sc_errors,
    "retry_abort": sc_retry_abort,
    "unreachable": sc_unreachable,
    "no_credentials": sc_no_credentials,
    "protocol_errors": sc_protocol_errors,
    "mode_json": sc_mode_json,
    "mode_print": sc_mode_print,
    "versions": sc_versions,
    "permission_gate": sc_permission_gate,
    "permission_gate_stock": sc_permission_gate_stock,
    "ui_methods": sc_ui_methods,
    "trust_and_context": sc_trust_and_context,
    "startup_flags": sc_startup_flags,
    "shutdown": sc_shutdown,
}


def listing(root):
    out = []
    for dp, _d, files in os.walk(root):
        for f in sorted(files):
            p = os.path.join(dp, f)
            out.append("# file %s %d bytes" % (os.path.relpath(p, root).replace("\\", "/"), os.path.getsize(p)))
    return sorted(out)


def run_scenario(name, template):
    base = os.path.join(SCRATCH, "run", name)
    shutil.rmtree(base, ignore_errors=True)
    ctx = {
        "home": os.path.join(base, "home"),
        "agentdir": os.path.join(base, "agentdir"),
        # outside any repository: pi reads AGENTS.md and CLAUDE.md from every parent directory
        "work": os.path.join(os.environ.get("PI_PROBE_WORKROOT", os.path.join(SCRATCH, "work")), name),
        "transcript": [],
    }
    os.makedirs(os.path.join(ctx["home"], "tmp"))
    shutil.rmtree(ctx["work"], ignore_errors=True)
    os.makedirs(ctx["work"])
    shutil.copytree(template, ctx["agentdir"])
    with open(os.path.join(ctx["work"], "notes.txt"), "w", newline="\n") as f:
        f.write("alpha beta\ngamma\n")
    with open(os.path.join(ctx["work"], "pic.png"), "wb") as f:
        f.write(PNG_1X1)
    ctx["transcript"].append("# scenario %s" % name)
    SC_START = os.path.getsize(REQLOG) if os.path.exists(REQLOG) else 0
    try:
        SCENARIOS[name](ctx)
    except Exception as e:  # keep what was recorded
        ctx["transcript"].append("# RECORDER ERROR %r" % (e,))
    ctx["transcript"].append("# agent dir after the scenario (relative):")
    ctx["transcript"] += ["# agentdir " + x[2:] for x in listing(ctx["agentdir"])]
    ctx["transcript"].append("# work dir after the scenario:")
    ctx["transcript"] += ["# work " + x[2:] for x in listing(ctx["work"])]
    ctx["transcript"].append("# auth.json: " + open(os.path.join(ctx["agentdir"], "auth.json"), encoding="utf-8").read().strip() if os.path.exists(os.path.join(ctx["agentdir"], "auth.json")) else "# no auth.json")
    # provider requests made during this scenario
    if os.path.exists(REQLOG):
        with open(REQLOG, "rb") as f:
            f.seek(SC_START)
            reqs = [json.loads(x) for x in f.read().decode("utf-8").split("\n") if x]
        ctx["_reqs"] = reqs
    text = "\n".join(ctx["transcript"]) + "\n"
    text = scrub(text, ctx)
    with open(os.path.join(OUT, name + ".txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    if ctx.get("_reqs"):
        rq = "\n".join(scrub(json.dumps(x, ensure_ascii=False), ctx) for x in ctx["_reqs"]) + "\n"
        with open(os.path.join(OUT, "provider_requests", name + ".jsonl"), "w", encoding="utf-8", newline="\n") as f:
            f.write(rq)


def main():
    os.makedirs(SCRATCH, exist_ok=True)
    os.makedirs(os.path.join(OUT, "provider_requests"), exist_ok=True)
    start_server()
    try:
        template = make_template(PORT)
        for name in (ONLY or list(SCENARIOS)):
            print("scenario", name, flush=True)
            run_scenario(name, template)
    finally:
        SERVER.kill()


if __name__ == "__main__":
    main()
