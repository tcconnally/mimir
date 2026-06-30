#!/usr/bin/env python3
"""Mimir LongMemEval session-level retrieval benchmark (offline, judge-free).

Measures whether Mimir *retrieves the gold evidence session* for each LongMemEval
question, the same session-level recall metric LongMemEval itself defines via
`answer_session_ids`. It is fully offline and deterministic: it drives the real
`mimir` binary over MCP stdio, ingests each question's haystack (one memory per
session, namespaced by question id), populates dense vectors with the **bundled**
ONNX model (no network, no API key, no LLM), and scores recall@k / MRR per search
mode (fts5 keyword, dense vector, hybrid RRF).

What this is and is NOT:
  * IS: a reproducible RETRIEVAL-quality number (does the right memory come back?).
    This is the judge-free half of LongMemEval and the half Mimir's local-first
    pitch can own: anyone can re-run it and get the same number with no API key.
  * IS NOT: end-to-end QA accuracy. That second stage feeds the retrieved context
    to an LLM and judges the answer; it needs an LLM + judge model and is therefore
    not offline/deterministic. Keep the two numbers separate and labeled. (See
    README.md. This harness deliberately does not invent a QA score.)

Dataset (real, public): LongMemEval `_s` split, 500 instances, ~48 sessions each
(~46 distractors + ~1.9 evidence). Download:
  curl -L https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_s_cleaned.json -o longmemeval_s_cleaned.json

Usage:
  python run.py --data /path/to/longmemeval_s_cleaned.json            # full 500
  python run.py --data ... --max-instances 50                          # quick subset
  python run.py --data ... --bin /path/to/mimir --k 1 3 5 10 --out report.json
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
    sys.exit("error: mimir binary not found. `cargo build --release` or pass --bin / set MIMIR_BIN.")


class MimirServer:
    """One persistent mimir process; many MCP tools/call over stdio.

    Process-per-call (as in ../recall/run.py) does not scale to LongMemEval's
    ~24k session writes, so we keep one process open and stream requests.
    """

    def __init__(self, binary, db):
        self.p = subprocess.Popen(
            [binary, "--db", db],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, encoding="utf-8", errors="replace",
        )
        self._id = 0
        self._send({"jsonrpc": "2.0", "id": self._next(), "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                               "clientInfo": {"name": "longmemeval", "version": "1"}}})
        self._read_result()  # initialize response
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _next(self):
        self._id += 1
        return self._id

    def _send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def _read_result(self):
        # Read lines until one carries a JSON-RPC result/error (skip notifications/logs).
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("mimir closed the stream unexpectedly")
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "result" in msg or "error" in msg:
                return msg

    def call(self, name, args):
        self._send({"jsonrpc": "2.0", "id": self._next(), "method": "tools/call",
                    "params": {"name": name, "arguments": args}})
        resp = self._read_result()
        r = resp.get("result", {})
        if isinstance(r, dict) and "content" in r:
            try:
                return json.loads(r["content"][0]["text"])
            except Exception:
                return r["content"][0]["text"]
        return resp

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=30)
        except Exception:
            self.p.kill()


def session_text(turns):
    """Flatten a LongMemEval session (list of {role, content}) into one string."""
    parts = []
    for t in turns:
        role = t.get("role", "")
        content = t.get("content", "")
        parts.append(f"{role}: {content}")
    return "\n".join(parts)


def recall_scores(ranked_keys, relevant, ks):
    rel = set(relevant)
    out = {f"recall@{k}": (1.0 if rel & set(ranked_keys[:k]) else 0.0) for k in ks}
    rr = 0.0
    for i, key in enumerate(ranked_keys, start=1):
        if key in rel:
            rr = 1.0 / i
            break
    out["rr"] = rr
    return out


def main():
    ap = argparse.ArgumentParser(description="Mimir LongMemEval session-level retrieval benchmark")
    ap.add_argument("--data", required=True, help="Path to longmemeval_s_cleaned.json")
    ap.add_argument("--bin", default=None)
    ap.add_argument("--max-instances", type=int, default=0, help="0 = all 500")
    ap.add_argument("--k", nargs="+", type=int, default=[1, 3, 5, 10])
    ap.add_argument("--modes", nargs="+", default=["fts5", "dense", "hybrid"],
                    help="Search modes. Use 'auto' to send an empty mode and exercise "
                         "#271's auto-selection (the real default user experience).")
    ap.add_argument("--skip-explicit-embed", action="store_true",
                    help="Do not call mimir_embed; rely on #271 auto-embed-on-write. "
                         "Combined with mode 'auto', this is exactly what a user gets "
                         "from a bare remember + recall.")
    ap.add_argument("--limit", type=int, default=10, help="Sessions requested per query")
    ap.add_argument("--out", default=str(HERE / "report.json"))
    args = ap.parse_args()

    binary = find_binary(args.bin)
    data = json.loads(Path(args.data).read_text(encoding="utf-8"))
    if args.max_instances:
        data = data[: args.max_instances]
    ks = sorted(set(args.k))

    db_dir = Path(os.environ.get("TMPDIR") or os.environ.get("TEMP") or "/tmp")
    db = str(db_dir / "mimir-longmemeval.db")

    def wipe_db():
        for ext in ("", "-wal", "-shm"):
            try:
                os.remove(db + ext)
            except OSError:
                pass

    t0 = time.time()
    agg = {m: {f"recall@{k}": 0.0 for k in ks} | {"mrr": 0.0} for m in args.modes}
    by_type = {}
    per_q = []
    n_sessions_total = 0

    # Process-per-instance with a fresh tiny DB: each question only ever sees its
    # own ~48-session haystack, so per-instance cost stays constant (no growing-DB
    # slowdown) and isolation is exact. ~500 process launches total.
    for idx, inst in enumerate(data):
        qid = inst["question_id"]
        qtype = inst.get("question_type", "unknown")
        question = inst["question"]
        evidence = inst.get("answer_session_ids", [])
        sessions = inst.get("haystack_sessions", [])
        sids = inst.get("haystack_session_ids", [])

        wipe_db()
        srv = MimirServer(binary, db)
        try:
            # 1. Ingest this instance's sessions.
            for sid, turns in zip(sids, sessions):
                srv.call("mimir_remember", {
                    "category": qid, "key": sid,
                    "body_json": json.dumps({"note": session_text(turns)}),
                    "type": "fact",
                })
            n_sessions_total += len(sessions)

            # 2. Embed (bundled ONNX, offline) for dense/hybrid. With #271 every
            # write already auto-embeds, so --skip-explicit-embed measures the
            # bare remember+recall path real users actually hit.
            if not args.skip_explicit_embed:
                srv.call("mimir_embed", {"batch_category": qid, "batch_limit": 1000})

            # 3. Query per mode, score session-level recall.
            row = {"question_id": qid, "question_type": qtype, "evidence": evidence, "modes": {}}
            for mode in args.modes:
                # "auto" sends an empty mode so the server picks per #271.
                recall_mode = "" if mode == "auto" else mode
                r = srv.call("mimir_recall", {
                    "query": question, "mode": recall_mode, "category": qid,
                    "limit": args.limit, "trust_weight": 0, "min_decay": 0,
                })
                items = r.get("items", []) if isinstance(r, dict) else []
                ranked = [it.get("key") for it in items]
                s = recall_scores(ranked, evidence, ks)
                row["modes"][mode] = {"top": ranked[: max(ks)], **s}
                for k in ks:
                    agg[mode][f"recall@{k}"] += s[f"recall@{k}"]
                agg[mode]["mrr"] += s["rr"]
                bt = by_type.setdefault(qtype, {m: {f"recall@{k}": 0.0 for k in ks} | {"n": 0} for m in args.modes})
                for k in ks:
                    bt[mode][f"recall@{k}"] += s[f"recall@{k}"]
            for mode in args.modes:
                by_type[qtype][mode]["n"] += 1
            per_q.append(row)
        finally:
            srv.close()

        if (idx + 1) % 25 == 0:
            el = time.time() - t0
            print(f"  {idx+1}/{len(data)} instances  ({el:.0f}s, {n_sessions_total} sessions)", flush=True)
    wipe_db()

    n = len(data)
    for mode in args.modes:
        for key in agg[mode]:
            agg[mode][key] = round(agg[mode][key] / n, 4)
    for qt in by_type:
        for mode in args.modes:
            cnt = by_type[qt][mode]["n"] or 1
            for k in ks:
                by_type[qt][mode][f"recall@{k}"] = round(by_type[qt][mode][f"recall@{k}"] / cnt, 4)

    sig_payload = json.dumps({"dataset": "longmemeval_s", "n": n, "k": ks,
                              "modes": args.modes, "metrics": agg}, sort_keys=True)
    signature = hashlib.sha256(sig_payload.encode("utf-8")).hexdigest()

    report = {
        "benchmark": "mimir-longmemeval-retrieval",
        "metric": "session-level recall@k vs answer_session_ids (judge-free, offline)",
        "dataset": "longmemeval_s_cleaned.json",
        "n_instances": n,
        "n_sessions_ingested": n_sessions_total,
        "k": ks,
        "modes": args.modes,
        "limit": args.limit,
        "metrics": agg,
        "by_question_type": by_type,
        "binary": Path(binary).name,
        "platform": platform.platform(),
        "offline": True,
        "embedding": {"source": "bundled-onnx"},
        "elapsed_secs": round(time.time() - t0, 1),
        "signature_sha256": signature,
        "per_question": per_q,
    }
    Path(args.out).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    print(f"\nMimir LongMemEval session-level retrieval - {n} instances, {n_sessions_total} sessions")
    hdr = "mode    " + "".join(f"  R@{k:<4}" for k in ks) + "  MRR"
    print(hdr)
    print("-" * len(hdr))
    for mode in args.modes:
        cells = "".join(f"  {agg[mode][f'recall@{k}']*100:4.1f}" for k in ks)
        print(f"{mode:<7}{cells}  {agg[mode]['mrr']:.3f}")
    print(f"\nsignature: {signature[:16]}...  ->  {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
