"""Runs the shipped permission gate in a real pi 1.5.2 against the loopback stand-in (openai_chat_model.py), answering each way.

Usage: gate_probe.py SCRATCH PI_CLI_JS NODE GATE_TS STANDIN_PY (the stand-in is openai_chat_model.py)
Nothing real is used: no login, no provider, no network beyond 127.0.0.1.
"""
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time

SCRATCH, CLI, NODE, GATE, STANDIN = sys.argv[1:6]
PORT = 18931
agent = os.path.join(SCRATCH, "probe_agent")
work = os.path.join(SCRATCH, "probe_work")
home = os.path.join(SCRATCH, "probe_home")
for d in (agent, work, home):
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
open(os.path.join(work, "notes.txt"), "w").write("alpha beta\ngamma\n")
models = {"providers": {"stub": {
    "baseUrl": "http://127.0.0.1:%d/v1" % PORT, "api": "openai-completions", "apiKey": "sk-test",
    "compat": {"supportsReasoningEffort": True, "thinkingFormat": "openai"},
    "models": [
        {"id": "stub-fast", "name": "Stub Fast", "input": ["text"], "contextWindow": 128000, "maxTokens": 4096},
        {"id": "stub-think", "name": "Stub Think", "reasoning": True, "input": ["text", "image"],
         "contextWindow": 32000, "maxTokens": 4096, "cost": {"input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 1.25}}]}}}
json.dump(models, open(os.path.join(agent, "models.json"), "w"))
json.dump({"defaultProvider": "stub", "defaultModel": "stub-think", "defaultThinkingLevel": "medium"},
          open(os.path.join(agent, "settings.json"), "w"))
reqlog = os.path.join(SCRATCH, "probe_requests.jsonl")
open(reqlog, "w").close()
server = subprocess.Popen([sys.executable, STANDIN, str(PORT), reqlog], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
server.stdout.readline()
promptfile = os.path.join(SCRATCH, "probe_system.md")
open(promptfile, "w").write("PROBE-APPENDED-SYSTEM-TEXT")

env = {k: v for k, v in os.environ.items() if not any(s in k.upper() for s in ("KEY", "TOKEN", "SECRET", "PASSWORD"))}
env.update(PI_CODING_AGENT_DIR=agent, PI_SKIP_VERSION_CHECK="1", PI_OFFLINE="1", NO_COLOR="1",
           HOME=home, USERPROFILE=home, APPDATA=home, LOCALAPPDATA=home, TEMP=home, TMP=home)
env["PATH"] = os.path.dirname(NODE) + os.pathsep + env.get("PATH", "")
pi = subprocess.Popen([NODE, CLI, "--mode", "rpc", "-e", GATE, "--append-system-prompt", promptfile],
                      cwd=work, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
lines = queue.Queue()


def pump(stream, tag):
    for line in stream:
        lines.put((tag, line.rstrip("\n")))
    lines.put((tag, None))


threading.Thread(target=pump, args=(pi.stdout, "out"), daemon=True).start()
threading.Thread(target=pump, args=(pi.stderr, "err"), daemon=True).start()
dump = open(os.path.join(SCRATCH, "gate_live.stdout"), "w", newline="\n")


def record(event):
    """Keeps what pi wrote, without its system message and message lists, as the crate's fixtures have it."""
    if event.get("type") in ("message_start", "message_end") and event.get("message", {}).get("role") == "system":
        return
    if event.get("type") == "agent_end":
        event["messages"] = []
    dump.write(json.dumps(event, separators=(",", ":")) + "\n")


counter = [0]


def send(**message):
    counter[0] += 1
    message.setdefault("id", str(counter[0]))
    pi.stdin.write(json.dumps(message) + "\n")
    pi.stdin.flush()
    return message["id"]


def run(text, answers, images=None, label=None):
    """Sends a prompt and answers each gate dialog with the next of `answers`; returns the events until settled."""
    fields = {"type": "prompt", "message": text}
    if images:
        fields["images"] = images
    send(**fields)
    seen = []
    deadline = time.time() + 40
    while time.time() < deadline:
        try:
            tag, line = lines.get(timeout=1)
        except queue.Empty:
            continue
        if line is None:
            seen.append({"closed": tag})
            break
        if tag == "err":
            seen.append({"stderr": line})
            continue
        try:
            event = json.loads(line)
        except ValueError:
            seen.append({"raw": line})
            continue
        seen.append(event)
        record(event)
        if event.get("type") == "extension_ui_request" and event.get("method") == "select":
            answer = answers.pop(0)
            if answer == "cancel":
                send(type="extension_ui_response", id=event["id"], cancelled=True)
            else:
                send(type="extension_ui_response", id=event["id"], value=answer)
        if event.get("type") == "agent_settled":
            break
    print("==", label or text, answers)
    for e in seen:
        t = e.get("type")
        if t == "extension_ui_request":
            print("  DIALOG", json.dumps({k: e[k] for k in ("method", "title", "options") if k in e}))
        elif t == "tool_execution_start":
            print("  START", e.get("toolCallId"), e.get("toolName"))
        elif t == "tool_execution_end":
            print("  END", e.get("toolCallId"), e.get("toolName"), e.get("isError"), json.dumps(e["result"]["content"])[:200])
        elif t in ("agent_settled",):
            print("  SETTLED", e)
        elif t == "response":
            print("  RESPONSE", e.get("command"), e.get("success"), e.get("error", ""), json.dumps(e.get("data"))[:120])
        elif "stderr" in e or "raw" in e or "closed" in e:
            print("  OTHER", e)
    return seen


try:
    for who in ("allow", "deny", "plan", "cancel", "bogus"):
        run("[[bash:echo gated]] run", [who], label="bash answered " + who)
    run("[[write:probe.txt]] write", ["allow"], label="write allowed")
    print("probe.txt exists:", os.path.exists(os.path.join(work, "probe.txt")))
    run("[[two:notes.txt]] two calls", ["allow", "deny"], label="two calls: allow then deny (order of dialogs as they come)")
    png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
    run("", [], images=[{"type": "image", "data": png, "mimeType": "image/png"}], label="an image and an EMPTY message")
    send(type="get_state")
    time.sleep(1)
    while not lines.empty():
        tag, line = lines.get()
        if line and '"get_state"' in line:
            data = json.loads(line)["data"]
            print("state ok, model", data["model"]["id"], "level", data["thinkingLevel"])
finally:
    try:
        pi.stdin.close()
    except OSError:
        pass
    time.sleep(0.5)
    pi.kill()
    server.kill()
    reqs = [json.loads(l) for l in open(reqlog) if l.strip()]
    print("provider requests:", len(reqs))
    for r in reqs[:1]:
        body = r.get("body", r)
        text = json.dumps(body)
        print("appended system text reached the provider:", "PROBE-APPENDED-SYSTEM-TEXT" in text)
