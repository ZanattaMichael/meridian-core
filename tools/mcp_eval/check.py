#!/usr/bin/env python3
"""Scores a set of answers to the MCP model test set, through the server.

Each task in tasks.json is a request in plain words. A run of the test set
asks a model to answer each with a Meridian document, with the MCP server
attached or not, and saves the answer as <answers>/<task id>.yaml. This
script then asks `meridian mcp` to compile each answer and reports, per
answer, whether it compiles, whether it uses the resource types a correct
answer uses, and whether it says the things the task asked for. It needs no
model: any client's answers can be scored.

    python3 tools/mcp_eval/check.py --answers runs/small-with-server
    python3 tools/mcp_eval/check.py --answers runs/small-without --json

`--meridian` names the binary; by default `meridian` on the PATH. The
plugins are found as the binary finds them (`MERIDIAN_PLUGIN_DIR`).
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def rpc(proc: subprocess.Popen, id_: int, method: str, params: dict) -> dict:
    message = {"jsonrpc": "2.0", "id": id_, "method": method, "params": params}
    assert proc.stdin and proc.stdout
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()
    while True:
        line = proc.stdout.readline()
        if not line:
            raise SystemExit("the server closed its output")
        reply = json.loads(line)
        if reply.get("id") == id_:
            if "error" in reply:
                raise SystemExit(f"{method}: {reply['error']}")
            return reply["result"]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--answers", required=True, type=Path, help="directory of <task id>.yaml answers")
    parser.add_argument("--tasks", type=Path, default=Path(__file__).with_name("tasks.json"))
    parser.add_argument("--meridian", default="meridian", help="the meridian binary")
    parser.add_argument("--json", action="store_true", help="print the scores as JSON")
    args = parser.parse_args(argv[1:])

    tasks = json.loads(args.tasks.read_text())
    proc = subprocess.Popen(
        [args.meridian, "mcp", "--root", str(args.answers)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    rpc(proc, 1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}})
    scores = []
    next_id = 2
    for task in tasks:
        answer = args.answers / f"{task['id']}.yaml"
        score = {"id": task["id"], "target": task["target"], "answered": answer.is_file()}
        if answer.is_file():
            text = answer.read_text()
            result = rpc(proc, next_id, "tools/call", {"name": "compile", "arguments": {"document": text}})
            next_id += 1
            body = result["structuredContent"]
            score["compiles"] = bool(body.get("ok"))
            score["errors"] = [d["code"] for d in body.get("diagnostics", []) if d["severity"] == "error"]
            score["expectedTypes"] = all(f"type: {t}" in text for t in task["expectTypes"])
            score["expectedText"] = all(t in text for t in task.get("expectText", []))
            score["target"] = any(d.get("target") == task["target"] for d in body.get("documents", [])) or f"target: {task['target']}" in text
        scores.append(score)
    proc.stdin.close()
    proc.wait()

    if args.json:
        print(json.dumps(scores, indent=2))
    else:
        answered = [s for s in scores if s["answered"]]
        print(f"{'task':32} {'compiles':9} {'types':6} {'text':5} errors")
        for s in scores:
            if not s["answered"]:
                print(f"{s['id']:32} (no answer)")
                continue
            print(
                f"{s['id']:32} {str(s['compiles']):9} {str(s['expectedTypes']):6} "
                f"{str(s['expectedText']):5} {', '.join(s['errors'])}"
            )
        if answered:
            compiled = sum(1 for s in answered if s["compiles"]) / len(answered)
            typed = sum(1 for s in answered if s["expectedTypes"] and s["expectedText"]) / len(answered)
            print(f"\n{len(answered)} of {len(scores)} answered; {compiled:.0%} compile; {typed:.0%} use the expected types and say what was asked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
