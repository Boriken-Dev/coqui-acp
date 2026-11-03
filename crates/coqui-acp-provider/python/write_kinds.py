"""Writes `coqui_acp_provider_kinds.py` from the kinds' description, or with `--check` says whether it is
current (exit 1 if not). Needs Guatiao's Python binding on `PYTHONPATH`.

Guatiao's Python classes keep the name `close` for a remote's own method and find a slot by position, so
this kind's `close` slot is `close_` in Python, which `python -m guatiao.kinds` would not write.
"""

from __future__ import annotations

import json
import pathlib
import sys

from guatiao.kinds import generate

HERE = pathlib.Path(__file__).resolve().parent
DOCUMENT = HERE.parent / "include" / "coqui_acp_provider.kinds.json"
MODULE = HERE / "coqui_acp_provider_kinds.py"
RENAMED = {"close": "close_"}


def written() -> str:
    document = json.loads(DOCUMENT.read_text(encoding="utf-8"))
    for kind in document["kinds"]:
        for slot in kind["slots"]:
            slot["name"] = RENAMED.get(slot["name"], slot["name"])
    return generate(
        json.dumps(document),
        source="include/coqui_acp_provider.kinds.json",
        command="python python/write_kinds.py",
    )


def main() -> int:
    text = written()
    if "--check" in sys.argv[1:]:
        if MODULE.read_bytes().decode("utf-8") != text:
            print(f"{MODULE.name} is not what the description writes: regenerate it", file=sys.stderr)
            return 1
        return 0
    MODULE.write_bytes(text.encode("utf-8"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
