#!/usr/bin/env python3
"""Mimir bi-temporal benchmark — does time-travel return the right version?

Drives the real `mimir` binary over MCP stdio through fact-update scenarios
(write v1, then overwrite with v2 under the same category+key, which supersedes
v1 into history) and checks the bi-temporal contract:

  * as_of(mid)    returns the version that was live BETWEEN the two writes (v1),
  * as_of(now)    returns the current version (v2),
  * as_of(before) reports the fact did not exist yet (found=false),
  * current recall is LIVE-ONLY — the superseded v1 content never resurfaces,
  * current recall still finds the live v2 content.

Fully offline: no network, no API key, no LLM. Wall-clock timestamps separate
the two writes, so absolute times vary run-to-run, but the PASS/FAIL verdicts
(and the signature over them) are stable for a correct implementation.

Usage:
    cargo build --release
    python benchmark/temporal/run.py                       # score, write report.json
    python benchmark/temporal/run.py --bin /path/to/mimir --dataset other.json
    MIMIR_BIN=/path/to/mimir python benchmark/temporal/run.py

Exit code is non-zero if any check fails, so CI can gate on it.
"""
import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent


def find_binary(explicit):
    cands = []
    if explicit:
        cands.append(explicit)
    if os.environ.get("MIMIR_BIN"):
        cands.append(os.environ["MIMIR_BIN"])
    exe = "mneme.exe" if os.name == "nt" else "mneme"
    cands += [str(REPO / "target" / "release" / exe), str(REPO / "target" / "debug" / exe)]
    for c in cands:
        if c and Path(c).exists():
            return str(Path(c).resolve())
    sys.exit("error: mimir binary not found. Build it (`cargo build --release`) "
             "or pass --bin / set MIMIR_BIN.")


class Mimir:
    """One MCP tools/call per process (matches the sibling recall harness);
    state persists because every call points at the same --db file."""

    def __init__(self, binary, db):
        self.binary, self.db = binary, db

    def call(self, name, args):
        p = subprocess.Popen([self.binary, "--db", self.db], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        w = p.stdin.write
        w(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "temporal-bench", "version": "1"}}}) + "\n")
        p.stdin.flush()
        p.stdout.readline()
        w(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        p.stdin.flush()
        w(json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                      "params": {"name": name, "arguments": args}}) + "\n")
        p.stdin.flush()
        line = p.stdout.readline()
        p.stdin.close()
        p.wait(timeout=120)
        resp = json.loads(line)
        r = resp.get("result", {})
        if isinstance(r, dict) and "content" in r:
            try:
                return json.loads(r["content"][0]["text"])
            except Exception:
                return r["content"][0]["text"]
        return resp


def now_ms():
    return int(time.time() * 1000)


def main():
    ap = argparse.ArgumentParser(description="Mimir bi-temporal benchmark")
    ap.add_argument("--bin", default=None)
    ap.add_argument("--dataset", default=str(HERE / "dataset.json"))
    ap.add_argument("--out", default=str(HERE / "report.json"))
    ap.add_argument("--gap-ms", type=int, default=150,
                    help="sleep between v1 and the mid mark, and between the mid mark and v2")
    args = ap.parse_args()

    binary = find_binary(args.bin)
    data = json.loads(Path(args.dataset).read_text(encoding="utf-8"))
    updates = data["updates"]

    db_dir = Path(os.environ.get("TMPDIR") or os.environ.get("TEMP") or "/tmp")
    db = str(db_dir / "mimir-temporal-bench.db")
    for ext in ("", "-wal", "-shm"):
        try:
            os.remove(db + ext)
        except OSError:
            pass
    m = Mimir(binary, db)

    checks = []

    def record(scn, check, ok):
        checks.append({"scenario": scn, "check": check, "ok": bool(ok)})

    gap = args.gap_ms / 1000.0
    for u in updates:
        cat, key = u["category"], u["key"]
        t_before = now_ms() - 60_000  # comfortably before the fact existed

        m.call("mimir_remember", {"category": cat, "key": key,
                                  "body_json": json.dumps({"note": u["v1"]}), "type": "fact"})
        time.sleep(gap)
        t_mid = now_ms()             # an instant strictly between the two writes
        time.sleep(gap)
        m.call("mimir_remember", {"category": cat, "key": key,
                                  "body_json": json.dumps({"note": u["v2"]}), "type": "fact"})
        t_now = now_ms() + 60_000    # comfortably after

        a_mid = m.call("mimir_as_of", {"category": cat, "key": key, "as_of_unix_ms": t_mid})
        record(key, "as_of_mid_returns_v1",
               isinstance(a_mid, dict) and a_mid.get("found") and u["v1_token"] in json.dumps(a_mid))

        a_now = m.call("mimir_as_of", {"category": cat, "key": key, "as_of_unix_ms": t_now})
        record(key, "as_of_now_returns_v2",
               isinstance(a_now, dict) and a_now.get("found") and u["v2_token"] in json.dumps(a_now))

        a_before = m.call("mimir_as_of", {"category": cat, "key": key, "as_of_unix_ms": t_before})
        record(key, "as_of_before_not_found",
               isinstance(a_before, dict) and a_before.get("found") is False)

        r = m.call("mimir_recall", {"query": u["probe"], "mode": "fts5", "limit": 10,
                                    "trust_weight": 0, "min_decay": 0})
        bodies = json.dumps(r.get("items", []) if isinstance(r, dict) else [])
        record(key, "recall_excludes_superseded_v1", u["v1_token"] not in bodies)
        record(key, "recall_includes_live_v2", u["v2_token"] in bodies)

    total = len(checks)
    passed = sum(1 for c in checks if c["ok"])
    by_check = {}
    for c in checks:
        b = by_check.setdefault(c["check"], {"pass": 0, "total": 0})
        b["total"] += 1
        b["pass"] += 1 if c["ok"] else 0
    accuracy = round(passed / total, 4) if total else 1.0

    sig_payload = json.dumps(
        {"dataset": data.get("name"),
         "checks": [{"s": c["scenario"], "c": c["check"], "ok": c["ok"]} for c in checks]},
        sort_keys=True)
    signature = hashlib.sha256(sig_payload.encode("utf-8")).hexdigest()

    report = {
        "benchmark": "mimir-bi-temporal",
        "dataset": data.get("name"),
        "n_scenarios": len(updates),
        "checks_total": total,
        "checks_passed": passed,
        "accuracy": accuracy,
        "by_check": by_check,
        "binary": Path(binary).name,
        "platform": platform.platform(),
        "offline": True,
        "signature_sha256": signature,
        "results": checks,
    }
    Path(args.out).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    print(f"Mimir bi-temporal - {data.get('name')} ({len(updates)} scenarios, {total} checks)")
    for name, b in sorted(by_check.items()):
        mark = "ok " if b["pass"] == b["total"] else "FAIL"
        print(f"  [{mark}] {b['pass']}/{b['total']}  {name}")
    for c in checks:
        if not c["ok"]:
            print(f"    MISS [{c['scenario']}] {c['check']}")
    print(f"accuracy: {accuracy*100:.1f}%   signature: {signature[:16]}...  ->  {args.out}")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
