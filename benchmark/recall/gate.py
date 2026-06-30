#!/usr/bin/env python3
"""CI regression gate for the #271 invariant: semantic recall is the DEFAULT.

Runs the real mimir binary over MCP stdio on the in-repo recall dataset, using
exactly the calls a bare user makes — `mimir_remember` then `mimir_recall` with
NO manual `mimir_embed` and NO `mode` argument. It then asserts that this default
path retrieves far better than keyword-only FTS5. If a future change breaks
auto-embed-on-write or the hybrid default, recall collapses toward FTS5 and this
gate fails loudly.

This is the fast, dependency-free guard (24-memory dataset, no network, no API
key); the full LongMemEval proof lives in ../longmemeval/. Thresholds are relative
and conservative on purpose, so normal ranking drift does not cause flakes.

Exit 0 on pass, 1 on failure. Usage: python benchmark/recall/gate.py [--bin PATH]
"""
import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent

# Conservative invariants. The default (auto) path must clearly beat keyword-only.
MIN_AUTO_RECALL_AT_5 = 0.80      # default path must find the answer in top 5 most of the time
MIN_AUTO_OVER_FTS5_AT_5 = 0.20   # and must beat FTS5 by a wide, semantic margin
MIN_AUTO_MRR = 0.80              # and must rank the answer HIGH, not just somewhere in top 5
# The default path is hybrid (dense + keyword RRF). RRF can dilute a strong dense
# ranking when the keyword arm is weak, so the default may sit slightly below
# pure dense — but it must never collapse far below it. This guards the fusion
# quality of the default path against a regression that silently degrades it
# toward the weak keyword arm (see the fusion-dilution follow-up issue).
MAX_AUTO_BELOW_DENSE_AT_5 = 0.15   # default recall@5 must stay within this of pure dense
MAX_AUTO_BELOW_DENSE_MRR = 0.15    # and within this of pure dense on MRR


def find_binary(explicit):
    cands = [explicit, os.environ.get("MIMIR_BIN")]
    exe = "mneme.exe" if os.name == "nt" else "mneme"
    cands += [str(REPO / "target" / "release" / exe), str(REPO / "target" / "debug" / exe)]
    for c in cands:
        if c and Path(c).exists():
            return str(Path(c).resolve())
    sys.exit("error: mimir binary not found (build it or pass --bin / set MIMIR_BIN).")


class Mimir:
    def __init__(self, binary, db):
        self.p = subprocess.Popen([binary, "--db", db], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, encoding="utf-8", errors="replace")
        self._id = 0
        self._send({"jsonrpc": "2.0", "id": self._n(), "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                               "clientInfo": {"name": "recall-gate", "version": "1"}}})
        self._read()
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _n(self):
        self._id += 1
        return self._id

    def _send(self, m):
        self.p.stdin.write(json.dumps(m) + "\n")
        self.p.stdin.flush()

    def _read(self):
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("mimir closed the stream")
            try:
                m = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "result" in m or "error" in m:
                return m

    def call(self, name, args):
        self._send({"jsonrpc": "2.0", "id": self._n(), "method": "tools/call",
                    "params": {"name": name, "arguments": args}})
        r = self._read().get("result", {})
        if isinstance(r, dict) and "content" in r:
            try:
                return json.loads(r["content"][0]["text"])
            except Exception:
                return r["content"][0]["text"]
        return r

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=30)
        except Exception:
            self.p.kill()


def recall_at(ranked, relevant, k):
    return 1.0 if set(relevant) & set(ranked[:k]) else 0.0


def reciprocal_rank(ranked, relevant):
    """1/rank of the first relevant hit (0 if none). Rewards ranking the answer
    high, not merely surfacing it somewhere in the top-k — which is exactly the
    quality RRF fusion can erode when the keyword arm is noisy."""
    rel = set(relevant)
    for i, key in enumerate(ranked):
        if key in rel:
            return 1.0 / (i + 1)
    return 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=None)
    ap.add_argument("--dataset", default=str(HERE / "dataset.json"))
    args = ap.parse_args()

    binary = find_binary(args.bin)
    data = json.loads(Path(args.dataset).read_text(encoding="utf-8"))
    memories, queries = data["memories"], data["queries"]

    db = str(Path(os.environ.get("TEMP") or "/tmp") / "mimir-recall-gate.db")
    for ext in ("", "-wal", "-shm"):
        try:
            os.remove(db + ext)
        except OSError:
            pass

    m = Mimir(binary, db)
    try:
        # Bare ingest. NO mimir_embed call: auto-embed-on-write (#271) must populate vectors.
        for mem in memories:
            m.call("mimir_remember", {"category": mem["category"], "key": mem["key"],
                                      "body_json": json.dumps({"note": mem["note"]}), "type": "fact"})
        auto5 = fts5 = dense5 = 0.0
        auto_mrr = dense_mrr = 0.0
        for q in queries:
            # default path: no mode -> server auto-selects (#271)
            ra = m.call("mimir_recall", {"query": q["q"], "limit": 5, "trust_weight": 0, "min_decay": 0})
            rf = m.call("mimir_recall", {"query": q["q"], "mode": "fts5", "limit": 5,
                                         "trust_weight": 0, "min_decay": 0})
            rd = m.call("mimir_recall", {"query": q["q"], "mode": "dense", "limit": 5,
                                         "trust_weight": 0, "min_decay": 0})
            auto = [it.get("key") for it in (ra.get("items", []) if isinstance(ra, dict) else [])]
            keyw = [it.get("key") for it in (rf.get("items", []) if isinstance(rf, dict) else [])]
            dens = [it.get("key") for it in (rd.get("items", []) if isinstance(rd, dict) else [])]
            auto5 += recall_at(auto, q["relevant"], 5)
            fts5 += recall_at(keyw, q["relevant"], 5)
            dense5 += recall_at(dens, q["relevant"], 5)
            auto_mrr += reciprocal_rank(auto, q["relevant"])
            dense_mrr += reciprocal_rank(dens, q["relevant"])
    finally:
        m.close()

    n = len(queries)
    auto5 /= n
    fts5 /= n
    dense5 /= n
    auto_mrr /= n
    dense_mrr /= n
    print(f"default(auto) recall@5 = {auto5:.3f}  MRR = {auto_mrr:.3f}   "
          f"dense recall@5 = {dense5:.3f}  MRR = {dense_mrr:.3f}   "
          f"fts5 recall@5 = {fts5:.3f}   (n={n}, {len(memories)} memories, no manual embed)")

    ok = True
    if auto5 < MIN_AUTO_RECALL_AT_5:
        print(f"FAIL: default recall@5 {auto5:.3f} < {MIN_AUTO_RECALL_AT_5} "
              f"(auto-embed-on-write or hybrid-default likely broken)")
        ok = False
    if auto5 - fts5 < MIN_AUTO_OVER_FTS5_AT_5:
        print(f"FAIL: default beats fts5 by only {auto5-fts5:.3f} < {MIN_AUTO_OVER_FTS5_AT_5} "
              f"(default path is not using semantic search)")
        ok = False
    if auto_mrr < MIN_AUTO_MRR:
        print(f"FAIL: default MRR {auto_mrr:.3f} < {MIN_AUTO_MRR} "
              f"(default path surfaces the answer but ranks it poorly)")
        ok = False
    # Fusion-quality guard: the default (hybrid) path may sit slightly below pure
    # dense, but a regression that collapses fusion toward the weak keyword arm
    # must fail loudly rather than ship green.
    if dense5 - auto5 > MAX_AUTO_BELOW_DENSE_AT_5:
        print(f"FAIL: default recall@5 {auto5:.3f} trails pure dense {dense5:.3f} by "
              f">{MAX_AUTO_BELOW_DENSE_AT_5} (hybrid fusion is diluting the dense ranking)")
        ok = False
    if dense_mrr - auto_mrr > MAX_AUTO_BELOW_DENSE_MRR:
        print(f"FAIL: default MRR {auto_mrr:.3f} trails pure dense {dense_mrr:.3f} by "
              f">{MAX_AUTO_BELOW_DENSE_MRR} (hybrid fusion is diluting the dense ranking)")
        ok = False
    if ok:
        print("PASS: the default path uses semantic search, ranks answers high, "
              "and fusion does not dilute the dense ranking.")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
