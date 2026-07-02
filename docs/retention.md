# Retention, Decay, and Forgetting

Perseus Vault forgets on purpose. This page documents exactly when a memory
fades, when it is archived, when it is deleted, and how to opt a memory out of
each stage. All numbers below are the shipped constants in `src/db.rs`; if this
page and the code disagree, the code wins and this page has a bug.

## The lifecycle at a glance

```
remember ──▶ active (buffer) ──▶ working ──▶ core        promotion by USE
                │
                │ idle time (Ebbinghaus decay)
                ▼
        decay_score < 0.05  ──▶ archived (auto)          forgetting by DISUSE
                                    │
                                    │ explicit `purge`
                                    ▼
                                 deleted (permanent)
```

Nothing is ever deleted automatically. Automatic forgetting stops at
**archived**, which is reversible; only an explicit `purge` deletes rows.

## Decay: forgetting by disuse

Every entity carries a `decay_score` in `[0.0, 1.0]` recomputed from idle time:

```
decay_score = e^(−idle / 7 days)
```

(`DECAY_HALF_LIFE_MS = 7 days` — the name is historical; the curve is `e^-x`,
so the score is ~0.37 after 7 idle days, not 0.5.)

Reference points:

| Idle time | decay_score |
|---|---|
| just accessed | 1.0 |
| 7 days | ~0.37 |
| 14 days | ~0.14 |
| ~21 days | 0.05 → **auto-archived** |

Being recalled resets the clock and additionally boosts the stored score by
`DECAY_BOOST = 0.25` (capped at 1.0), so memories that keep getting used stay
comfortably above the archive line.

## The archive threshold — one number, everywhere

`ARCHIVE_DECAY_THRESHOLD = 0.05`. An entity whose recomputed score falls below
it is archived with an `archive_reason` explaining why. The same constant is
shared by every path that forgets:

- `decay_tick` (the explicit decay pass),
- `cohere` (the coherence groomer's gentle ×0.95 decay step),
- `autocohere`'s compact step.

This is deliberate: before v2.12.x, `autocohere` compacted at a hardcoded 0.1
(~16 idle days) while the individual tools used 0.05 (~21 days), so running
"everything" forgot ~5 days sooner than any single tool.

## Exemptions: how a memory opts out of forgetting

| Mechanism | Effect |
|---|---|
| `verified: true` | `decay_score` floored at `VERIFIED_DECAY_FLOOR = 0.2` — a verified fact can fade but is **never auto-archived**. |
| `always_on: true` | Injected unconditionally into `context`/`prepare` blocks regardless of decay; being injected does not itself bump retrieval stats. |
| regular use | Every recall boosts the score by 0.25 and resets the idle clock. |

The verified floor exists because curated facts match few queries and are
rarely recall-boosted; without it they decayed below 0.05 and were silently
forgotten while chatty low-value memories that match everything stayed hot
(#298).

## Layers: promotion by use

Layer is a function of `retrieval_count`, shared by the recall side-effect
path and `cohere`'s promotion step (unified in v2.12.x — cohere previously
promoted at 3 while recall promoted at 5, so 3–4-retrieval entities
oscillated):

| Layer | Threshold |
|---|---|
| `buffer` | fewer than 5 retrievals (`WORKING_THRESHOLD`) |
| `working` | ≥ 5 retrievals |
| `core` | ≥ 20 retrievals (`CORE_THRESHOLD`) |

Layers affect ranking and `recall_layer` filtering; they do not change the
decay math.

## Archived is not deleted

Archived entities keep their row, body, links, and history. They are excluded
from recall (unless `include_archived` is set) and from `context`/`prepare`
injection. Recovery is a `remember` to the same `(category, key)` or manual
un-archiving.

Deletion is explicit and two-step:

- **`prune`** — archive (not delete) entities matching filters you choose
  (category, `decay_score` below a cutoff, older than N days).
- **`purge`** — permanently delete entities that are **already archived**.
  Supports `dry_run`. This is the only way memory leaves the database.

## Known limitation

Retrieval reinforcement currently fires only on the keyword (`fts5`) recall
path; the default hybrid/dense path is side-effect-free to keep recall
byte-deterministic over a frozen DB (#247, see
`deterministic-recall-and-provenance.md`). A memory that is only ever found
via hybrid recall therefore still decays as if unused. Whether to trade that
determinism for reinforcement-on-use is an open product decision; until it is
made, mark load-bearing memories `verified` (floor) or `always_on`
(unconditional injection) rather than relying on usage to keep them alive.
