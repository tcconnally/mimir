#!/usr/bin/env python3
"""Mimir offline recall-quality benchmark.

Measures whether Mimir *retrieves the right memory*, not how fast it does so
(the latency/throughput suite lives in ../run.py). It is fully offline and
deterministic: it drives the real `mimir` binary over MCP stdio, ingests a
paraphrase-heavy dataset, populates dense vectors with the **bundled** ONNX
embedding model (no network, no API key, no LLM), and scores recall@k / MRR for
each search mode (fts5 keyword, dense vector, hybrid RRF).

Usage:
    python run.py                       # auto-locate the binary, score, write report.json
    python run.py --bin /path/to/mimir  # explicit binary
    python run.py --dataset other.json --k 1 3 5 --out report.json
    MIMIR_BIN=/path/to/mimir python run.py

Plugging in the real benchmarks: pass --dataset pointing at a JSON file with the
same shape ({"memories": [...], "queries": [{"q", "relevant": [keys]}]}) built
from LOCOMO or LongMemEval. The harness is dataset-agnostic.
"""
import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent


def find_binary(explicit: "str | None") -> str:
    candidates = []
    if explicit:
        candidates.append(explicit)
    if os.environ.get("MIMIR_BIN"):
        candidates.append(os.environ["MIMIR_BIN"])
    exe = "mimir.exe" if os.name == "nt" else "mimir"
    candidates += [str(REPO / "target" / "release" / exe),
                   str(REPO / "target" / "debug" / exe)]
    for c in candidates:
        if c and Path(c).exists():
            return str(Path(c).resolve())
    sys.exit("error: mimir binary not found. Build it (`cargo build --release`) "
             "or pass --bin / set MIMIR_BIN.")


class Mimir:
    """One MCP tools/call per process (matches the sibling perf harness).

    State persists because every call points at the same --db file.
    """

    def __init__(self, binary: str, db: str):
        self.binary = binary
        self.db = db

    def call(self, name: str, args: dict):
        p = subprocess.Popen([self.binary, "--db", self.db],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True)
        w = p.stdin.write
        w(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "recall-bench", "version": "1"}}}) + "\n")
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


def score(ranked_keys, relevant, ks):
    """recall@k for each k, plus reciprocal rank (0 if no hit)."""
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
    ap = argparse.ArgumentParser(description="Mimir offline recall-quality benchmark")
    ap.add_argument("--bin", default=None, help="Path to the mimir binary")
    ap.add_argument("--dataset", default=str(HERE / "dataset.json"))
    ap.add_argument("--k", nargs="+", type=int, default=[1, 3, 5])
    ap.add_argument("--modes", nargs="+", default=["fts5", "dense", "hybrid"])
    ap.add_argument("--out", default=str(HERE / "report.json"))
    ap.add_argument("--limit", type=int, default=10, help="Results requested per query")
    args = ap.parse_args()

    binary = find_binary(args.bin)
    data = json.loads(Path(args.dataset).read_text(encoding="utf-8"))
    memories, queries = data["memories"], data["queries"]
    ks = sorted(set(args.k))

    db_dir = Path(os.environ.get("TMPDIR") or os.environ.get("TEMP") or "/tmp")
    db = str(db_dir / "mimir-recall-bench.db")
    for ext in ("", "-wal", "-shm"):
        try:
            os.remove(db + ext)
        except OSError:
            pass

    m = Mimir(binary, db)

    # 1. Ingest.
    print(f"Ingesting {len(memories)} memories...", flush=True)
    for mem in memories:
        m.call("mimir_remember", {
            "category": mem["category"], "key": mem["key"],
            "body_json": json.dumps({"note": mem["note"]}), "type": "fact",
        })

    # 2. Populate dense vectors with the bundled local model (no network).
    cats = sorted({mem["category"] for mem in memories})
    embedded, dims = 0, None
    for cat in cats:
        rep = m.call("mimir_embed", {"batch_category": cat, "batch_limit": 1000})
        if isinstance(rep, dict):
            embedded += int(rep.get("embedded", 0) or 0)
            dims = rep.get("dimensions", dims)
    dim_note = f", {dims}-dim" if dims else ""
    print(f"Embedded {embedded} entities (bundled ONNX{dim_note}).", flush=True)

    # 3. Query each mode and score.
    agg = {mode: {f"recall@{k}": 0.0 for k in ks} | {"mrr": 0.0} for mode in args.modes}
    per_query = []
    for q in queries:
        row = {"q": q["q"], "relevant": q["relevant"], "modes": {}}
        for mode in args.modes:
            r = m.call("mimir_recall", {"query": q["q"], "mode": mode, "limit": args.limit,
                                        "trust_weight": 0, "min_decay": 0})
            items = r.get("items", []) if isinstance(r, dict) else []
            ranked = [it.get("key") for it in items]
            s = score(ranked, q["relevant"], ks)
            row["modes"][mode] = {"top": ranked[:max(ks)], **s}
            for k in ks:
                agg[mode][f"recall@{k}"] += s[f"recall@{k}"]
            agg[mode]["mrr"] += s["rr"]
        per_query.append(row)

    n = len(queries)
    for mode in args.modes:
        for key in agg[mode]:
            agg[mode][key] = round(agg[mode][key] / n, 4)

    # Signature over the *reproducible* modes only. fts5 and dense are
    # deterministic run-to-run; `hybrid` (RRF) is NOT — its tie ordering depends
    # on wall-clock decay and on mimir_recall's access side-effects, so its
    # recall@k drifts by ~1-2 queries between runs. We therefore exclude it from
    # the pinned signature but still report its scores (advisory). See README.
    NONDETERMINISTIC = {"hybrid"}
    repro_modes = [m for m in args.modes if m not in NONDETERMINISTIC]
    sig_payload = json.dumps({
        "dataset": data.get("name"), "k": ks, "modes": repro_modes,
        "metrics": {m: agg[m] for m in repro_modes},
    }, sort_keys=True)
    signature = hashlib.sha256(sig_payload.encode("utf-8")).hexdigest()

    report = {
        "benchmark": "mimir-recall-quality",
        "dataset": data.get("name"),
        "n_memories": len(memories),
        "n_queries": n,
        "k": ks,
        "modes": args.modes,
        "metrics": agg,
        "binary": Path(binary).name,
        "platform": platform.platform(),
        "offline": True,
        "embedding": {"source": "bundled-onnx", "embedded": embedded, "dimensions": dims},
        "signature_sha256": signature,
        "signature_covers": repro_modes,
        "nondeterministic_modes": sorted(NONDETERMINISTIC & set(args.modes)),
        "per_query": per_query,
    }
    Path(args.out).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    # Human summary.
    print(f"\nMimir recall quality - {data.get('name')} ({n} queries, {len(memories)} memories)")
    hdr = "mode    " + "".join(f"  R@{k:<5}" for k in ks) + "  MRR"
    print(hdr)
    print("-" * len(hdr))
    for mode in args.modes:
        cells = "".join(f"  {agg[mode][f'recall@{k}']*100:5.1f}" for k in ks)
        print(f"{mode:<7}{cells}  {agg[mode]['mrr']:.3f}")
    print(f"\nsignature: {signature[:16]}...  ->  {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
