#!/usr/bin/env python3
"""Resumes an imported session in the real command line tool, against a stand-in model, and prints what it sent the model.

    python3 -I tools/record_harnesses/resume_check.py claude --codec coqui-acp-claude --history FILE [--text TEXT]
    python3 -I tools/record_harnesses/resume_check.py codex  --codec coqui-acp-codex  --history FILE [--text TEXT]

`--history` is a history in the standard format. The codec (the `coqui-acp-claude` or `coqui-acp-codex` program, built with its `history` feature; its
`history import` subcommand) imports it into a home made from nothing under the root, the real tool resumes
the session from that home and is given one more prompt, and the messages the stand-in model received for it are printed
as JSON: the context the tool resumed with, which is what an import must get right. No model is reached, nothing is
spent, and nothing of the person's is seen.
"""

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

import record

SESSION = "6f2c1b0e-0000-4000-8000-0000000c0a9c"


def import_into(codec, args, history):
    with open(history, "rb") as handle:
        done = subprocess.run([codec, "history", "import", *args], stdin=handle, capture_output=True, check=False)
    if done.returncode != 0:
        sys.exit(f"the codec refused: {done.stderr.decode('utf-8', 'replace')}")
    return done.stderr.decode("utf-8", "replace").strip()


def model_requests(logs):
    log = logs / "model.jsonl"
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines() if line.strip()]


def check_claude(args, root):
    for name in ("w", "c", "h", "t", "l"):
        (root / name).mkdir(exist_ok=True)
    logs = root / "l"
    notes = import_into(args.codec, ["--config", str(root / "c"), str(root / "w"), args.id], args.history)
    model, port = record.start_model("anthropic", [{"text": "resumed answer"}], logs)
    env = record.base_env(root)
    env.update({"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}", "ANTHROPIC_API_KEY": "dummy-stand-in-key",
                "CLAUDE_CONFIG_DIR": str(root / "c"), "DISABLE_TELEMETRY": "1", "DISABLE_AUTOUPDATER": "1",
                "DISABLE_ERROR_REPORTING": "1", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"})
    git_bash = pathlib.Path("C:/Program Files/Git/bin/bash.exe")
    if record.WINDOWS and git_bash.exists():
        env["CLAUDE_CODE_GIT_BASH_PATH"] = str(git_bash)
    argv = ["-p", "--input-format=stream-json", "--output-format=stream-json", "--verbose", "--replay-user-messages",
            f"--resume={args.id}", "--setting-sources=", "--strict-mcp-config", "--model=claude-haiku-5-5"]
    tool = record.Tool([args.program] + argv, root / "w", env, root / "l" / "out.jsonl", lambda *_: None)
    case = {"steps": [{"send": {"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": args.text}]}}},
                      {"until": {"type": "result"}, "timeout": 200}, {"close": True}]}
    seen = []
    try:
        record.run_steps(tool, case, {}, seen.append)
    finally:
        tool.finish()
        model.kill()
        model.wait()
    sent = [r for r in model_requests(logs) if r.get("method") == "POST" and r.get("main")]
    return {"import": notes, "requests": len(sent), "messages": sent[-1]["input"] if sent else None}


def check_codex(args, root):
    for name in ("w", "home", "h", "t", "l"):
        (root / name).mkdir(exist_ok=True)
    logs = root / "l"
    notes = import_into(args.codec, ["--config", str(root / "home"), str(root / "w"), args.id], args.history)
    model, port = record.start_model("responses", [{"text": "resumed answer"}], logs)
    provider = {"base_url": f"http://127.0.0.1:{port}/v1", "env_key": "STANDIN_API_KEY", "name": "standin",
                "wire_api": "responses"}
    table = "{" + ",".join(f"{json.dumps(k)}={json.dumps(provider[k])}" for k in sorted(provider)) + "}"
    argv = ["app-server", '--config=model_provider="standin"', f"--config=model_providers.standin={table}"]
    env = record.base_env(root)
    env.update({"CODEX_HOME": str(root / "home"), "STANDIN_API_KEY": "dummy-stand-in-key"})
    seen = []
    tool = record.Tool([args.program] + argv, root / "w", env, root / "l" / "out.jsonl", record.codex_answer([]),
                       end_tree_on_close=True)
    result = {"import": notes}
    try:
        tool.send({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "coqui-acp-codex", "version": "0.0.0"}, "capabilities": {}}})
        tool.until({"id": 1}, 60, seen.append)
        tool.send({"method": "initialized", "params": {}})
        tool.send({"id": 2, "method": "thread/resume", "params": {"threadId": args.id, "cwd": str(root / "w"),
                                                                    "approvalPolicy": "never", "sandbox": "danger-full-access"}})
        tool.until({"id": 2}, 60, seen.append)
        answer = [m for m in seen if m.get("id") == 2][-1]
        thread = answer.get("result", {}).get("thread", {})
        result["resumed"] = {"error": answer.get("error"), "id": thread.get("id"),
                             "turns": [{"status": t.get("status"), "items": [i.get("type") for i in t.get("items", [])]}
                                       for t in thread.get("turns", [])]}
        tool.send({"id": 3, "method": "turn/start", "params": {"threadId": thread.get("id") or args.id,
                                                                "input": [{"type": "text", "text": args.text}]}})
        tool.until({"method": "turn/completed"}, 120, seen.append)
        time.sleep(0.5)
    finally:
        tool.finish()
        model.kill()
        model.wait()
    sent = [r for r in model_requests(logs) if r.get("method") == "POST"]
    result["requests"] = len(sent)
    result["messages"] = sent[-1]["input"] if sent else None
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("harness", choices=["claude", "codex"])
    parser.add_argument("--codec", required=True, help="the coqui-acp-claude or coqui-acp-codex program, built with --features history")
    parser.add_argument("--history", required=True)
    parser.add_argument("--program", help="the tool, by default found on PATH")
    parser.add_argument("--id", default=SESSION, help="the session's UUID")
    parser.add_argument("--text", default="And then?")
    parser.add_argument("--out", help="write the JSON here instead of standard output")
    parser.add_argument("--root", default="C:/coqui-fx" if record.WINDOWS else str(pathlib.Path(tempfile.gettempdir()) / "coqui-fx"))
    args = parser.parse_args()
    args.program = args.program or shutil.which(args.harness)
    if not args.program:
        parser.error(f"{args.harness} is not on PATH")
    root = pathlib.Path(args.root)
    record.clear(root)
    root.mkdir(parents=True)
    try:
        result = (check_claude if args.harness == "claude" else check_codex)(args, root)
    finally:
        record.clear(root)
    text = json.dumps(result, indent=1, ensure_ascii=False)
    if args.out:
        pathlib.Path(args.out).write_text(text + "\n", encoding="utf-8", newline="")
    else:
        print(text)


if __name__ == "__main__":
    main()
