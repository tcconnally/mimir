#!/usr/bin/env python3
"""Mimir benchmark suite — publishable latency / throughput results.

Drives the real `mimir` binary over MCP stdio and measures write throughput,
recall latency, category filtering, decay ordering, journal throughput,
near-duplicate detection, and vault export.

Runs anywhere: the binary is auto-located (repo `target/release|debug`, or
`--bin` / `MIMIR_BIN`), and all scratch paths are OS-temp based. A single
persistent mimir process serves every call, so the throughput numbers reflect
Mimir's actual per-op cost rather than process-spawn overhead.

Usage:
    cargo build --release
    python benchmark/run.py                 # auto-locate the binary, print + save report
    python benchmark/run.py --bin /path/to/mimir --out report.json
    MIMIR_BIN=/path/to/mimir python benchmark/run.py
"""
import argparse
import hashlib
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent


def find_binary(explicit):
    cands = [explicit, os.environ.get("MIMIR_BIN")]
    exe = "mneme.exe" if os.name == "nt" else "mneme"
    cands += [str(REPO / "target" / "release" / exe), str(REPO / "target" / "debug" / exe)]
    for c in cands:
        if c and Path(c).exists():
            return str(Path(c).resolve())
    sys.exit("error: mimir binary not found (build it or pass --bin / set MIMIR_BIN).")


class Mimir:
    """Persistent MCP stdio client — one process, many calls."""

    def __init__(self, binary, db):
        self.p = subprocess.Popen([binary, "--db", db], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, encoding="utf-8", errors="replace")
        self._id = 0
        self._send({"jsonrpc": "2.0", "id": self._n(), "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                               "clientInfo": {"name": "bench", "version": "1.0"}}})
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

    def call(self, name, args=None):
        self._send({"jsonrpc": "2.0", "id": self._n(), "method": "tools/call",
                    "params": {"name": name, "arguments": args or {}}})
        resp = self._read()
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=None)
    # Defaults to OS temp (the curated benchmark/results.json is a hand-annotated
    # artifact and must not be clobbered by a raw run). Pass --out to capture.
    ap.add_argument("--out", default=str(Path(tempfile.gettempdir()) / "mimir" / "benchmark" / "results.json"))
    ap.add_argument("--writes", type=int, default=10000)
    args = ap.parse_args()

    binary = find_binary(args.bin)
    db = str(Path(tempfile.gettempdir()) / "mimir-bench.db")
    for ext in ("", "-wal", "-shm"):
        try:
            os.remove(db + ext)
        except OSError:
            pass

    rpc = Mimir(binary, db)
    results = {}
    cats = ["decision", "architecture", "convention", "insight", "fact"]
    total_writes = args.writes

    try:
        # ── 1. Write Throughput ──
        print(f"1. Writing {total_writes:,} entities...", end=" ", flush=True)
        t0 = time.perf_counter()
        for i in range(total_writes):
            # Each body carries a unique deterministic nonce so near-duplicate
            # detection does not collapse the bulk insert. Without it the bodies
            # ("Entity 0 in decision", "Entity 1 in decision", ...) are trigram
            # near-dupes and dedup rejects almost all of them, so the throughput
            # and entity-count numbers measure dedup rejections, not inserts.
            nonce = hashlib.sha1(f"mimir-bench-{i}".encode()).hexdigest()
            rpc.call("mimir_remember", {
                "category": cats[i % 5], "key": f"bench-{i}",
                "body_json": json.dumps({"id": i, "desc": f"Entity {i} in {cats[i%5]}",
                                         "tag": f"tag-{i%20}", "nonce": nonce}),
                "type": cats[i % 5], "importance": 0.5 + (i % 5) * 0.1
            })
        elapsed = time.perf_counter() - t0
        results["write"] = {"count": total_writes, "elapsed_s": round(elapsed, 1),
                            "docs_per_sec": round(total_writes / elapsed)}
        print(f"{total_writes/elapsed:.0f} docs/sec")

        # ── 2. Recall Latency ──
        print("2. Recall latency (100 queries)...", end=" ", flush=True)
        times = []
        for i in range(100):
            t0 = time.perf_counter()
            rpc.call("mimir_recall", {"query": f"entity {i*100}", "limit": 10})
            times.append((time.perf_counter() - t0) * 1000)
        times.sort()
        results["recall"] = {"p50_ms": round(statistics.median(times), 1),
                            "p99_ms": round(times[99], 1),
                            "avg_ms": round(statistics.mean(times), 1)}
        print(f"p50={results['recall']['p50_ms']}ms")

        # ── 3. Category Precision ──
        print("3. Category-filtered recall...", end=" ", flush=True)
        dec = rpc.call("mimir_recall", {"query": "entity", "category": "decision", "limit": 100})
        arc = rpc.call("mimir_recall", {"query": "entity", "category": "architecture", "limit": 100})
        all_cats = all(rpc.call("mimir_recall", {"query": "entity", "category": c, "limit": 1})["total"] > 0 for c in cats)
        results["category_filter"] = {"decision_hits": dec["total"], "architecture_hits": arc["total"],
                                      "all_categories_match": all_cats}
        print(f"decision={dec['total']}, architecture={arc['total']}")

        # ── 4. Decay ──
        print("4. Decay accuracy...", end=" ", flush=True)
        rpc.call("mimir_remember", {"category": "bench", "key": "fresh", "body_json": "{\"d\":\"fresh\"}", "importance": 1.0})
        rpc.call("mimir_remember", {"category": "bench", "key": "stale", "body_json": "{\"d\":\"stale\"}", "importance": 0.1})
        for _ in range(10):
            rpc.call("mimir_recall", {"query": "fresh", "limit": 1})
        fresh = rpc.call("mimir_recall", {"query": "fresh", "limit": 1})["items"][0]
        stale = rpc.call("mimir_recall", {"query": "stale", "limit": 1})["items"][0]
        results["decay"] = {"fresh_score": fresh["decay_score"], "stale_score": stale["decay_score"],
                            "fresh_layer": fresh["layer"], "stale_layer": stale["layer"],
                            "fresh_ranks_higher": fresh["decay_score"] > stale["decay_score"]}
        print("ok" if results["decay"]["fresh_ranks_higher"] else "FAIL")

        # ── 5. Journal ──
        print("5. Journal writes (1000 events)...", end=" ", flush=True)
        t0 = time.perf_counter()
        for i in range(1000):
            rpc.call("mimir_journal", {"event_type": "bench", "evaluated": {"i": i}, "acted": {"ok": True}, "forward": {"n": i + 1}})
        elapsed = time.perf_counter() - t0
        results["journal"] = {"count": 1000, "elapsed_s": round(elapsed, 1),
                              "events_per_sec": round(1000 / elapsed)}
        print(f"{1000/elapsed:.0f} events/sec")

        # ── 6. Dedup ──
        print("6. Near-duplicate detection...", end=" ", flush=True)
        rpc.call("mimir_remember", {"category": "test", "key": "orig", "body_json": "{\"unique\":\"content for dedup test 12345\"}", "importance": 0.8})
        dup = rpc.call("mimir_remember", {"category": "test", "key": "copy", "body_json": "{\"unique\":\"content for dedup test 12345\"}", "importance": 0.8})
        # Match the action substring: current Mimir returns "deduped (new key not
        # created)" rather than the bare "deduped" this check originally expected.
        results["dedup"] = {"detected": "deduped" in (dup.get("action") or ""), "action": dup.get("action")}
        print(results["dedup"]["action"])

        # ── 7. Vault Export ──
        print("7. Vault export...", end=" ", flush=True)
        vd = tempfile.mkdtemp()
        t0 = time.perf_counter()
        rpc.call("mimir_vault_export", {"vault_dir": vd})
        elapsed = time.perf_counter() - t0
        fc = len([f for f in os.listdir(vd) if f.endswith('.md')])
        results["vault"] = {"files": fc, "elapsed_s": round(elapsed, 2),
                            "files_per_sec": round(fc / max(elapsed, 0.001))}
        import shutil
        shutil.rmtree(vd)
        print(f"{fc} files in {elapsed:.1f}s")

        # ── 8. DB Stats ──
        stats = rpc.call("mimir_stats", {})
        results["db"] = {"entities": stats["total_entities"], "journal": stats["total_journal_events"],
                        "size_kb": round(stats["db_file_size_bytes"] / 1024),
                        "categories": len(stats["by_category"]),
                        "layers": stats["by_layer"]}
        print(f"8. DB: {stats['total_entities']} entities, {stats['db_file_size_bytes']/1024:.0f}KB")
    finally:
        rpc.close()

    # ── Output ──
    print("\n" + "=" * 55)
    for k, v in results.items():
        print(f"  {k}: {json.dumps(v)}")

    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(results, indent=2), encoding="utf-8")
    print(f"\nSaved: {args.out}")

    try:
        os.remove(db)
    except OSError:
        pass


if __name__ == "__main__":
    main()
