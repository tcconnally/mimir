# LongMemEval — see `benchmark/longmemeval/` (this file is deprecated)

> **Deprecated 2026-06-28.** The benchmark now lives at
> [`benchmark/longmemeval/`](../benchmark/longmemeval/README.md): a reproducible,
> fully offline, judge-free **session-level retrieval** harness that drives the
> real `mimir` binary over the public LongMemEval `_s` split and emits a signed
> `report.json`.

## Why the previous results here were removed

The earlier results in this file were **not reproducible** and should not be
cited:

- They reported a model named `google/gemma-4-26b-a4b-it`, which does not exist
  (Google's Gemma line does not have a "4 / a4b" release; that naming is not a real
  model). A number whose model cannot be identified cannot be trusted.
- The headline comparison mixed **different LLMs, judges, and splits** in one table
  (Mimir on a 50-sample oracle split vs published systems on a 102-sample stratified
  split with a different LLM and judge). The file itself admitted "direct comparison
  should use identical LLM/judge," which means the comparison was not valid.
- The reproduce steps referenced a harness (`scripts/run_full_benchmark.py`) that is
  not in this repository, so no one could re-run it.

## What replaced it

`benchmark/longmemeval/` measures the half of LongMemEval that is honest to claim
offline: **retrieval quality** (does Mimir surface the gold evidence session?),
using LongMemEval's own `answer_session_ids` metric, reproducible by anyone with no
API key. End-to-end QA accuracy (which needs an LLM + judge) is intentionally kept
separate and is not asserted here until it can be run with named models across all
baselines on an identical split.
