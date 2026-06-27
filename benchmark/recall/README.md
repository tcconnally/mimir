# Mimir recall-quality benchmark

A **reproducible, fully offline** measurement of whether Mimir retrieves the
*right* memory — recall@k and MRR — across its three search modes. This is a
**quality** benchmark; the latency/throughput suite lives in
[`../run.py`](../run.py).

> **Why this exists.** The agent-memory field's recall numbers are notoriously
> unreproducible (the same system has been reported at wildly different LOCOMO
> scores across sources). Mimir's pitch is local-first/offline, so its
> credibility benchmark should be one anyone can re-run on their own machine
> with **no API key, no network, no LLM** — and get the same number. That is
> what this harness is.

## Run it

```bash
cargo build --release            # builds mimir with bundled embeddings (default)
python benchmark/recall/run.py   # auto-locates target/release/mimir
```

Or point at a binary explicitly:

```bash
python benchmark/recall/run.py --bin /path/to/mimir
MIMIR_BIN=/path/to/mimir python benchmark/recall/run.py
```

It writes [`report.json`](./report.json) and prints a summary. Exit code is 0
on success.

## How it works

1. Ingest the dataset's memories via `mimir_remember`.
2. Populate dense vectors with the **bundled** ONNX model via `mimir_embed`
   (local, no network, no API key).
3. For each query, call `mimir_recall` in each mode (`fts5`, `dense`, `hybrid`)
   and score recall@k / reciprocal rank against the query's known-relevant keys.

Everything runs against the **real shipped binary over MCP stdio** — the same
path a production agent uses — so the numbers reflect what users actually get.

## The dataset

[`dataset.json`](./dataset.json) — `mimir-recall-mini`, a 24-memory /
24-query personal-assistant set in the LOCOMO / LongMemEval mold. It is
deliberately **paraphrase-heavy**: each query is worded differently from the
memory that answers it (e.g. *"does the user own any animals"* → *"I have a
golden retriever named Max"*), so keyword-only search is stressed and semantic
retrieval is rewarded. Domain-adjacent distractors are included.

It is intentionally small and self-contained so the benchmark needs no download.
To run the **full** public benchmarks, pass a dataset of the same shape built
from LOCOMO or LongMemEval — the harness is dataset-agnostic:

```bash
python benchmark/recall/run.py --dataset locomo_subset.json
```

```json
{"memories": [{"category": "...", "key": "...", "note": "..."}],
 "queries":  [{"q": "...", "relevant": ["key1"]}]}
```

## Results (this dataset, committed [`report.json`](./report.json))

| Mode | recall@1 | recall@3 | recall@5 | MRR |
|---|---|---|---|---|
| `fts5` (keyword) | 4.2% | 12.5% | 20.8% | 0.131 |
| `dense` (bundled embeddings) | **91.7%** | **95.8%** | **100%** | **0.948** |
| `hybrid` (RRF) † | 20.8% | 54.2% | 83.3% | 0.431 |

*Measured on `mimir.exe`, Windows 11, bundled int8 all-MiniLM-L6-v2. Your
absolute numbers may differ slightly by platform/binary; the methodology and the
relative picture are the point.*

### Honest findings

- **Bundled local embeddings carry recall.** On paraphrased queries, keyword
  search alone is near-useless (4.2% recall@1) — it cannot match *"own any
  animals"* to *"golden retriever"*. The offline dense model gets it right 92% of
  the time at rank 1 and **100% within the top 5**, with **zero network calls**.
  That is the local-first promise made measurable.
- **This set is adversarial to keyword search by design.** A real corpus has
  some lexically-overlapping queries where `fts5` does fine; don't read 4.2% as
  Mimir's keyword quality in general — read it as "paraphrase needs semantics."
- **† `hybrid` underperforms pure `dense` here, and is non-deterministic.**
  Mimir's hybrid mode fuses keyword + dense via Reciprocal Rank Fusion. When the
  keyword arm is near-useless (as on this paraphrase set), RRF *dilutes* the
  strong dense ranking, dropping recall@1 from 92% to ~21%. Worse, hybrid's
  ranking drifts ~1–2 queries run-to-run because its tie ordering depends on
  wall-clock decay and on `mimir_recall`'s access side-effects. **`fts5` and
  `dense` are byte-stable run-to-run; `hybrid` is not** — so the pinned
  `signature_sha256` covers only the reproducible modes (`signature_covers`).
  This is a real improvement target: query-relevance-aware RRF weighting (down-
  weight the keyword arm when it has no strong hits) and a deterministic,
  read-only benchmark recall path.

## Reproducibility

`fts5` and `dense` metrics are deterministic for a given dataset + binary +
platform; re-running yields an identical `signature_sha256` over those modes.
Exact dense rankings can vary marginally across CPU architectures (ONNX
floating-point), so treat the committed `report.json` as the reference for *this*
platform; CI (Linux) is the canonical re-run when wired up.
