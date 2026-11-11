#!/usr/bin/env python3
"""Records the fixtures of coqui-acp-claude and coqui-acp-codex from the real command line tools.

    python3 -I tools/record_harnesses/record.py claude [--program claude] [--case NAME ...]
    python3 -I tools/record_harnesses/record.py codex  [--program codex]  [--case NAME ...]

Each case in cases/<harness>/ runs the tool against a stand-in model on 127.0.0.1, so no model is reached and nothing is
spent, from an environment built from nothing, so no login or setting of the person's is seen. It writes, into the
crate's tests/fixtures/<version>/, `<case>.stdout` (what the tool wrote, the bytes as written but for the machine's time zone and name), `<case>.steps.json` (each
line written to it, after how many output lines) and `<case>.argv.json` (its arguments, the root and port as placeholders,
which tests/argv.rs compares with what the crate's launch builds).

After a new version of a tool: record into a new directory, run the crate's tests against it, and read what changed.

A case is JSON: "argv"; "script", the stand-in's steps (see anthropic_model.py, responses_model.py); "steps", each of
{"send": line}, {"until": {key: value}, "timeout": seconds}, {"sleep": seconds} or {"close": true}; "policy", the answers
to permission requests, in order; "files", text by path under the root; "env" and "env_remove"; "seed", a case or a case's name run first
in the same root, unrecorded; "transcript", whether to copy the session's messages to `<case>.transcript.jsonl`; "rollout" (Codex), whether to copy its
rollout file to `<case>.rollout.jsonl`;
"provider_extra", more of Codex's stand-in provider; "mcp", whether to write the root's mcp.json naming mcp_echo.py.
"""

import argparse
import json
import os
import pathlib
import platform
import queue
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time

HERE = pathlib.Path(__file__).resolve().parent
REPOSITORY = HERE.parent.parent
WINDOWS = platform.system() == "Windows"
# A limit's message names the machine's time zone, whatever TZ says on Windows.
TIME_ZONE = re.compile(rb"\([A-Z][A-Za-z_]+/[A-Za-z_/]+\)")
HOST_NAME = platform.node().encode("utf-8")


def scrubbed(line):
    """What a tool says of the machine that is no part of what is recorded: its time zone and its name."""
    return TIME_ZONE.sub(b"(UTC)", line).replace(HOST_NAME, b"standin-host")


def clear(root):
    """Empties the root, waiting for what the last case's tool still holds there to let go: on Windows, up to some tens of seconds."""
    def writable(remove, path, _error):
        # Codex keeps read-only files in its home.
        os.chmod(path, stat.S_IWRITE)
        remove(path)

    for _ in range(600):
        try:
            shutil.rmtree(root, onerror=writable)
        except OSError:
            pass
        if not root.exists():
            return
        time.sleep(0.2)
    raise SystemExit(f"{root} cannot be removed")


def write(path, text):
    """Writes text with LF line ends on every platform."""
    path.write_bytes(text.encode("utf-8"))


def neutral_path():
    """A PATH of the system's own directories: what a tool prints of it must not be this machine's."""
    if WINDOWS:
        return "C:/Windows/System32;C:/Windows;C:/Windows/System32/WindowsPowerShell/v1.0"
    return "/usr/bin:/bin"


def fill(value, values):
    text = json.dumps(value)
    for key, replacement in values.items():
        text = text.replace("{" + key + "}", json.dumps(replacement)[1:-1])
    return json.loads(text)


def start_model(kind, script, logs):
    """Starts a stand-in model, returning it and its port."""
    script_file = logs / "script.json"
    write(script_file, json.dumps(script))
    port_file = logs / "port"
    port_file.unlink(missing_ok=True)
    model = subprocess.Popen([sys.executable, "-I", str(HERE / f"{kind}_model.py"), str(port_file),
                              str(logs / "model.jsonl"), str(script_file)])
    for _ in range(400):
        if port_file.exists() and port_file.read_text().strip():
            break
        time.sleep(0.05)
    return model, port_file.read_text().strip()


def base_env(root):
    home = root / "h"
    env = {"PATH": neutral_path(), "HOME": str(home), "USERPROFILE": str(home), "APPDATA": str(home / "AppData" / "Roaming"),
           "LOCALAPPDATA": str(home / "AppData" / "Local"), "TEMP": str(root / "t"), "TMP": str(root / "t"),
           "TMPDIR": str(root / "t"), "TZ": "UTC"}
    for name in ("SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "SYSTEMDRIVE", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS", "OS"):
        if name in os.environ:
            env[name] = os.environ[name]
    return env


class Tool:
    """A running tool: what it writes is kept and queued, and what is written to it is noted."""

    def __init__(self, argv, cwd, env, out_file, answer, end_tree_on_close=False):
        self.process = subprocess.Popen(argv, cwd=str(cwd), env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, start_new_session=not WINDOWS)
        self.end_tree_on_close = end_tree_on_close
        self.lines = queue.Queue()
        self.count = 0
        self.written = []
        self.answer = answer
        self.raw = open(out_file, "wb")
        threading.Thread(target=self.pump, daemon=True).start()

    def pump(self):
        for line in self.process.stdout:
            self.raw.write(scrubbed(line))
            self.raw.flush()
            self.count += 1
            self.lines.put(line)
        self.lines.put(None)

    def send(self, line):
        self.written.append({"after": self.count, "input": line})
        self.process.stdin.write((json.dumps(line) + "\n").encode("utf-8"))
        self.process.stdin.flush()

    def until(self, want, timeout, seen):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                line = self.lines.get(timeout=0.5)
            except queue.Empty:
                continue
            if line is None:
                return False
            try:
                message = json.loads(line)
            except ValueError:
                continue
            seen(message)
            self.answer(self, message)
            if all(message.get(key) == value for key, value in want.items()):
                return True
        return False

    def end_tree(self):
        """Ends the tool and what it started: Codex's shells outlive it, and hold the root open."""
        if WINDOWS:
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(self.process.pid)], capture_output=True, check=False)
        elif self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGKILL)

    def finish(self):
        if self.process.poll() is None:
            self.end_tree()
        self.process.wait()
        time.sleep(0.3)
        self.raw.close()


def run_steps(tool, case, values, seen):
    for step in case["steps"]:
        if "send" in step:
            tool.send(fill(step["send"], values))
        elif "until" in step:
            if not tool.until(step["until"], step.get("timeout", 120), seen):
                print(f"  did not see {step['until']}")
                return False
        elif "sleep" in step:
            time.sleep(step["sleep"])
        elif "close" in step:
            if tool.end_tree_on_close:
                tool.end_tree()
                return True
            tool.process.stdin.close()
            try:
                tool.process.wait(30)
            except subprocess.TimeoutExpired:
                pass
    return True


LEFT_OUT = "[left out of the recording]"


def shortened(value):
    """`value` with every long text replaced: Codex's own instructions and tool descriptions, which no test reads."""
    if isinstance(value, str):
        return LEFT_OUT if len(value) > 1500 else value
    if isinstance(value, list):
        return [shortened(item) for item in value]
    if isinstance(value, dict):
        return {key: shortened(item) for key, item in value.items()}
    return value


def scrubbed_rollout(data):
    """A rollout as the recording keeps it: the machine's time zone and name removed, long texts left out, LF line ends."""
    lines = [json.dumps(shortened(json.loads(scrubbed(line))), ensure_ascii=False, separators=(",", ":"))
             for line in data.splitlines()]
    return ("\n".join(lines) + "\n").encode("utf-8")


def kept_line(entry):
    """A transcript line that is part of the conversation: its messages and where it was compacted."""
    return entry.get("type") in ("user", "assistant") or (entry.get("type") == "system" and entry.get("subtype") == "compact_boundary")


def claude_answer(policy):
    def answer(tool, message):
        request = message.get("request", {})
        if message.get("type") != "control_request" or request.get("subtype") != "can_use_tool":
            return
        choice = policy.pop(0) if policy else "deny"
        if choice == "deny":
            result = {"behavior": "deny", "message": "The person refused this."}
        else:
            result = {"behavior": "allow", "updatedInput": request.get("input", {})}
            if choice == "always" and request.get("permission_suggestions"):
                result["updatedPermissions"] = request["permission_suggestions"]
        tool.send({"type": "control_response", "response": {"subtype": "success", "request_id": message["request_id"],
                                                            "response": result}})
    return answer


def write_files(case, root, values):
    for name, text in case.get("files", {}).items():
        path = pathlib.Path(fill(name, values))
        path = path if path.is_absolute() else root / path
        path.parent.mkdir(parents=True, exist_ok=True)
        write(path, fill(text, values))
    if case.get("mcp"):
        write(root / "mcp.json", json.dumps({"mcpServers": {"echo": {"type": "stdio", "command": sys.executable,
                                                                    "args": ["-I", str(HERE / "mcp_echo.py")]}}}))


def record_claude(program, case, name, root, out, recording=True):
    logs = root / "l"
    logs.mkdir(parents=True, exist_ok=True)
    (root / "w").mkdir(exist_ok=True)
    (root / "c").mkdir(exist_ok=True)
    (root / "h").mkdir(exist_ok=True)
    (root / "t").mkdir(exist_ok=True)
    values = {"ROOT": root.as_posix(), "RESET": str(int(time.time()) + 3600)}
    write_files(case, root, values)
    write(root / "c" / "settings.json", json.dumps({"skipWebFetchPreflight": True}))
    model, port = start_model("anthropic", fill(case.get("script", []), values), logs)
    env = base_env(root)
    env.update({"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}", "ANTHROPIC_API_KEY": "dummy-stand-in-key",
                "CLAUDE_CONFIG_DIR": str(root / "c"), "DISABLE_TELEMETRY": "1", "DISABLE_AUTOUPDATER": "1",
                "DISABLE_ERROR_REPORTING": "1", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"})
    if WINDOWS:
        git_bash = "C:/Program Files/Git/bin/bash.exe"
        if pathlib.Path(git_bash).exists():
            env["CLAUDE_CODE_GIT_BASH_PATH"] = git_bash
    env.update(fill(case.get("env", {}), values))
    for variable in case.get("env_remove", []):
        env.pop(variable, None)
    assert re.fullmatch(r"http://127\.0\.0\.1:\d+", env["ANTHROPIC_BASE_URL"]), "the model must be the stand-in"
    argv = fill(case["argv"], values)
    out_file = out / f"{name}.stdout" if recording else logs / "seed.stdout"
    tool = Tool([program] + argv, root / "w", env, out_file, claude_answer(list(case.get("policy", []))))
    try:
        run_steps(tool, case, values, lambda message: None)
    finally:
        tool.finish()
        model.kill()
        model.wait()
    if recording:
        write(out / f"{name}.steps.json", json.dumps(tool.written, indent=1) + "\n")
        write(out / f"{name}.argv.json", json.dumps(case["argv"], indent=1) + "\n")
        if case.get("transcript"):
            found = next(iter((root / "c" / "projects").rglob(f"{case['transcript']}.jsonl")))
            kept = [line for line in found.read_bytes().splitlines(keepends=True) if kept_line(json.loads(line))]
            (out / f"{name}.transcript.jsonl").write_bytes(b"".join(kept))
    return tool.count


def codex_answer(policy):
    def answer(tool, message):
        method = message.get("method", "")
        if "id" not in message or not method:
            return
        if "requestApproval" in method or method in ("execCommandApproval", "applyPatchApproval"):
            decision = policy.pop(0) if policy else "decline"
            if method == "item/permissions/requestApproval":
                granted = message["params"].get("permissions", {}) if decision != "decline" else {}
                result = {"permissions": granted, "scope": "session" if decision == "acceptForSession" else "turn"}
            else:
                result = {"decision": decision}
            tool.send({"id": message["id"], "result": result})
        else:
            tool.send({"id": message["id"], "error": {"code": -32601, "message": "not handled"}})
    return answer


def record_codex(program, case, name, root, out, recording=True):
    logs = root / "l"
    logs.mkdir(parents=True, exist_ok=True)
    for directory in ("w", "home", "h", "t"):
        (root / directory).mkdir(exist_ok=True)
    values = {"ROOT": root.as_posix()}
    write_files(case, root, values)
    model, port = start_model("responses", fill(case.get("script", []), values), logs)
    values["PORT"] = port
    provider = {"base_url": f"http://127.0.0.1:{port}/v1", "env_key": "STANDIN_API_KEY", "name": "standin", "wire_api": "responses"}
    provider.update(case.get("provider_extra", {}))
    # As the crate's launch writes a table: its keys sorted and quoted.
    table = "{" + ",".join(f"{json.dumps(key)}={json.dumps(provider[key])}" for key in sorted(provider)) + "}"
    argv = ["app-server", '--config=model_provider="standin"', f"--config=model_providers.standin={table}"] + fill(case.get("argv", []), values)
    env = base_env(root)
    env.update({"CODEX_HOME": str(root / "home"), "STANDIN_API_KEY": "dummy-stand-in-key"})
    env.update(fill(case.get("env", {}), values))
    out_file = out / f"{name}.stdout" if recording else logs / "seed.stdout"
    tool = Tool([program] + argv, root / "w", env, out_file, codex_answer(list(case.get("policy", []))), end_tree_on_close=True)

    def seen(message):
        result = message.get("result")
        thread = result.get("thread", {}).get("id") if isinstance(result, dict) and isinstance(result.get("thread"), dict) else None
        if isinstance(result, dict) and not thread and isinstance(result.get("data"), list) and result["data"]:
            thread = result["data"][0].get("id") if isinstance(result["data"][0], dict) and "preview" in result["data"][0] else None
        if thread:
            values["THREAD"] = thread
        if message.get("method") == "turn/started":
            values["TURN"] = message["params"]["turn"]["id"]

    try:
        run_steps(tool, case, values, seen)
    finally:
        tool.finish()
        model.kill()
        model.wait()
    if recording:
        write(out / f"{name}.steps.json", json.dumps(tool.written, indent=1) + "\n")
        kept = [argument.replace(port, "{PORT}") for argument in argv]
        write(out / f"{name}.argv.json", json.dumps(kept, indent=1) + "\n")
        if case.get("rollout"):
            found = sorted((root / "home" / "sessions").rglob("rollout-*.jsonl"))
            assert len(found) == 1, f"{len(found)} rollout files"
            (out / f"{name}.rollout.jsonl").write_bytes(scrubbed_rollout(found[0].read_bytes()))
    return tool.count


def version_of(program):
    return subprocess.run([program, "--version"], capture_output=True, text=True, check=True).stdout.split()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("harness", choices=["claude", "codex"])
    parser.add_argument("--program", help="the tool, by default found on PATH")
    parser.add_argument("--case", action="append", help="only these cases")
    parser.add_argument("--out", help="where the recordings go, by default the crate's directory for this version")
    parser.add_argument("--root", default="C:/coqui-fx" if WINDOWS else os.path.join(tempfile.gettempdir(), "coqui-fx"),
                        help="the directory it runs in, deleted before each case: fixtures name it")
    args = parser.parse_args()
    program = args.program or shutil.which(args.harness)
    if not program:
        parser.error(f"{args.harness} is not on PATH")
    words = version_of(program)
    version = words[0] if args.harness == "claude" else f"codex-{words[1]}"
    out = pathlib.Path(args.out) if args.out else REPOSITORY / "crates" / f"coqui-acp-{args.harness}" / "tests" / "fixtures" / version
    out.mkdir(parents=True, exist_ok=True)
    record = record_claude if args.harness == "claude" else record_codex
    root = pathlib.Path(args.root)
    for file in sorted((HERE / "cases" / args.harness).glob("*.json")):
        name = file.stem
        if args.case and name not in args.case:
            continue
        case = json.loads(file.read_text(encoding="utf-8"))
        clear(root)
        root.mkdir(parents=True)
        if "seed" in case:
            seed = case["seed"]
            if isinstance(seed, str):
                seed = json.loads((HERE / "cases" / args.harness / f"{seed}.json").read_text(encoding="utf-8"))
            record(program, seed, name, root, out, recording=False)
        lines = record(program, case, name, root, out)
        print(f"{name}: {lines} lines")
    clear(root)


if __name__ == "__main__":
    main()
