#!/usr/bin/env python3
"""Mimir v0.5-rc Benchmark Suite — publishable results."""
import json, time, subprocess, os, statistics, tempfile, shutil

MIMIR = "/opt/data/webui/minions/.minions-data/mimir/mimir"
DB = "/tmp/mimir-bench.db"
if os.path.exists(DB):
    os.remove(DB)

def rpc(method, args=None):
    proc = subprocess.Popen([MIMIR, "--db", DB], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    proc.stdin.write(json.dumps({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"bench","version":"1.0"}}}) + "\n")
    proc.stdin.flush(); proc.stdout.readline()
    proc.stdin.write(json.dumps({"jsonrpc":"2.0","method":"notifications/initialized"}) + "\n")
    proc.stdin.flush()
    params = {"name": method, "arguments": args or {}}
    proc.stdin.write(json.dumps({"jsonrpc":"2.0","id":2,"method":"tools/call","params":params}) + "\n")
    proc.stdin.flush()
    resp = json.loads(proc.stdout.readline())
    proc.stdin.close(); proc.wait(timeout=10)
    if "result" in resp and "content" in resp["result"]:
        try: return json.loads(resp["result"]["content"][0]["text"])
        except: return resp["result"]["content"][0]["text"]
    return resp

results = {}
cats = ["decision","architecture","convention","insight","fact"]
total_writes = 10000

# ── 1. Write Throughput ──
print("1. Writing 10,000 entities...", end=" ", flush=True)
t0 = time.perf_counter()
for i in range(total_writes):
    rpc("mimir_remember", {
        "category": cats[i % 5], "key": f"bench-{i}",
        "body_json": json.dumps({"id": i, "desc": f"Entity {i} in {cats[i%5]}", "tag": f"tag-{i%20}"}),
        "type": cats[i % 5], "importance": 0.5 + (i % 5) * 0.1
    })
elapsed = time.perf_counter() - t0
results["write"] = {"count": total_writes, "elapsed_s": round(elapsed,1),
                     "docs_per_sec": round(total_writes/elapsed)}
print(f"{total_writes/elapsed:.0f} docs/sec")

# ── 2. Recall Latency ──
print("2. Recall latency (100 queries)...", end=" ", flush=True)
times = []
for i in range(100):
    t0 = time.perf_counter()
    rpc("mimir_recall", {"query": f"entity {i*100}", "limit": 10})
    times.append((time.perf_counter() - t0) * 1000)
results["recall"] = {"p50_ms": round(statistics.median(times),1),
                      "p99_ms": round(sorted(times)[99],1),
                      "avg_ms": round(statistics.mean(times),1)}
print(f"p50={results['recall']['p50_ms']}ms")

# ── 3. Category Precision ──
print("3. Category-filtered recall...", end=" ", flush=True)
dec = rpc("mimir_recall", {"query": "entity", "category": "decision", "limit": 100})
arc = rpc("mimir_recall", {"query": "entity", "category": "architecture", "limit": 100})
all_cats = all(rpc("mimir_recall", {"query": "entity", "category": c, "limit": 1})["total"] > 0 for c in cats)
results["category_filter"] = {"decision_hits": dec["total"], "architecture_hits": arc["total"],
                                "all_categories_match": all_cats}
print(f"decision={dec['total']}, architecture={arc['total']}")

# ── 4. Decay ──
print("4. Decay accuracy...", end=" ", flush=True)
rpc("mimir_remember", {"category":"bench","key":"fresh","body_json":"{\"d\":\"fresh\"}","importance":1.0})
rpc("mimir_remember", {"category":"bench","key":"stale","body_json":"{\"d\":\"stale\"}","importance":0.1})
for _ in range(10):
    rpc("mimir_recall", {"query":"fresh","limit":1})
fresh = rpc("mimir_recall", {"query":"fresh","limit":1})["items"][0]
stale = rpc("mimir_recall", {"query":"stale","limit":1})["items"][0]
results["decay"] = {"fresh_score": fresh["decay_score"], "stale_score": stale["decay_score"],
                     "fresh_layer": fresh["layer"], "stale_layer": stale["layer"],
                     "fresh_ranks_higher": fresh["decay_score"] > stale["decay_score"]}
print("ok" if results["decay"]["fresh_ranks_higher"] else "FAIL")

# ── 5. Journal ──
print("5. Journal writes (1000 events)...", end=" ", flush=True)
t0 = time.perf_counter()
for i in range(1000):
    rpc("mimir_journal", {"event_type":"bench","evaluated":{"i":i},"acted":{"ok":True},"forward":{"n":i+1}})
elapsed = time.perf_counter() - t0
results["journal"] = {"count": 1000, "elapsed_s": round(elapsed,1),
                       "events_per_sec": round(1000/elapsed)}
print(f"{1000/elapsed:.0f} events/sec")

# ── 6. Dedup ──
print("6. Near-duplicate detection...", end=" ", flush=True)
rpc("mimir_remember", {"category":"test","key":"orig","body_json":"{\"unique\":\"content for dedup test 12345\"}","importance":0.8})
dup = rpc("mimir_remember", {"category":"test","key":"copy","body_json":"{\"unique\":\"content for dedup test 12345\"}","importance":0.8})
results["dedup"] = {"detected": dup.get("action")=="deduped", "action": dup.get("action")}
print(results["dedup"]["action"])

# ── 7. Vault Export ──
print("7. Vault export...", end=" ", flush=True)
vd = tempfile.mkdtemp()
t0 = time.perf_counter()
exp = rpc("mimir_vault_export", {"vault_dir": vd})
elapsed = time.perf_counter() - t0
fc = len([f for f in os.listdir(vd) if f.endswith('.md')])
results["vault"] = {"files": fc, "elapsed_s": round(elapsed,2),
                     "files_per_sec": round(fc/max(elapsed,0.001))}
shutil.rmtree(vd)
print(f"{fc} files in {elapsed:.1f}s")

# ── 8. DB Stats ──
stats = rpc("mimir_stats", {})
results["db"] = {"entities": stats["total_entities"], "journal": stats["total_journal_events"],
                  "size_kb": round(stats["db_file_size_bytes"]/1024),
                  "categories": len(stats["by_category"]),
                  "layers": stats["by_layer"]}
print(f"8. DB: {stats['total_entities']} entities, {stats['db_file_size_bytes']/1024:.0f}KB")

# ── Output ──
print("\n" + "=" * 55)
for k, v in results.items():
    print(f"  {k}: {json.dumps(v)}")

out = "/tmp/mimir/benchmark/results.json"
os.makedirs(os.path.dirname(out), exist_ok=True)
with open(out, "w") as f:
    json.dump(results, f, indent=2)
print(f"\nSaved: {out}")
os.remove(DB)
