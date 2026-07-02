# Perseus Vault — Fable 5 Development Handoff

> Written 2026-07-02. Grounded in live repo state (verified via GitHub API, not memory recall).
> Current version: **v2.13.0** (Cargo.toml) / latest tag **v2.8.0** — Cargo.toml is ahead of the
> last pushed tag, so a version bump + release/tag pass is due.
> Open issues: **0**. Open PRs: **1** (#347 — Hamming-prefilter perf work for dense search at scale).

## Purpose of this doc

Development on Perseus, Perseus Vault, and Plutus is moving to **Fable 5** for
intensive work. This file is the entry point for that handoff.

## What Perseus Vault is (unchanged, per repo description)

Persistent memory MCP server for AI agents. SQLite + FTS5 + vector search,
AES-256-GCM at rest, single static Rust binary. Local-first, MIT. Formerly
known as Mneme/Mimir — rebrand is mid-flight, not yet fully swept.

## Verified current state (2026-07-02)

Recent merged work (last 5 commits on `main`, all landed 2026-07-02):
- `mimir_memories` — Anthropic `/memories` directory-convention adapter (#345)
- Persistent `importance` column — explicit scores now survive decay
  (fidelity prioritized over recency) (#344)
- Opt-in `reinforce` flag for dense/hybrid recall (#343)
- Workspace-scoped entity identity fix — share/federate now **copies**
  instead of clobbering across workspaces (#339/#342)
- Workspace-scope dashboard endpoint hardening + test coverage (#346)

Open PR #347 (Hamming-prefilter for dense search at scale) is the only
outstanding item — review/merge is the immediate next action, not new scoping.

## Phase roadmap for Fable 5

### Phase 1 — Version/release hygiene (do first, low effort)
- Cargo.toml says 2.13.0; last pushed tag is v2.8.0. Cut and push the
  missing tags (or confirm intentional — some point releases may not be
  tagged). Standing convention: bump VERSION, rebuild, commit before tagging;
  CI fails on stale build artifacts.
- Finish the Mneme/Mimir → Perseus Vault rebrand sweep in this repo's own
  docs (comparison pages, plugin manifests, awesome-list entries) — this was
  flagged as in-progress in an earlier session; verify current completion
  state against the repo rather than assuming it's done.

### Phase 2 — Workspace isolation for cross-domain memory (real bug, found live)
Live dogfooding on 2026-07-02 surfaced a concrete cross-domain leakage
problem: a single workspace mixes personal, health, and multiple unrelated
dev-project memories, all surfacing regardless of topical relevance to the
active conversation. Concrete plan:
1. Use the existing `workspace_hash` scoping (already shipped —
   `mimir_share`, `mimir_federate`, workspace-filtered recall) to split
   personal/health/finance contexts from dev/project contexts by default,
   rather than leaving everything unscoped in one shared workspace.
2. Where Perseus's context renderer is the actual consumer, pass the
   correct `workspace_hash` filter per session/profile type instead of
   pulling unscoped "recent + high-retrieval" entities blind.
3. Note: `retrieval_count` is currently being used as a de facto relevance
   signal (some entities show 70-97 retrievals purely from being pulled
   into unrelated conversations repeatedly). That's a scoring bug, separate
   from workspace scoping — track as a follow-up decay/scoring fix once
   workspace isolation ships, since isolation alone fixes most of the
   symptom.

### Phase 3 — Dense search perf (PR #347, already in flight)
Land the Hamming-prefilter for dense search at scale. This is the
highest-leverage open item since it's already implemented and awaiting
review — don't let it stall behind new roadmap work.

### Phase 4 — Public-sector / compliance track (standing priority)
Same north-star as Perseus: SBOM, NIST-mapped security whitepaper for the
AES-256-GCM implementation, PROV-O provenance tracking, journal
chain-of-custody hardening, decision-trace dashboard. These map to the
existing phase2_audit bucket in prior planning — no change, just confirming
Fable 5 should treat this as standing background work, not a one-off.

### Phase 5 — Billing integration gate (cross-repo dependency)
Per existing design intent: do **not** wire up Plutus billing for any hosted
tier until Plutus itself reaches a stable 1.0 (frozen API + DB schema). This
is already noted in this repo's own ROADMAP.md "Later — Gated" section —
repeating here so Fable 5 doesn't accidentally start that work early just
because Plutus is now tagged v1.0.0 (see Plutus handoff — 1.0.0 is
code-frozen but the tag/publish + external security review are still
pending as of this doc).

## What NOT to do
- Don't re-open #539-style "vault silently drops hits" concerns without
  checking `perseus` repo state first — that was a `perseus`-side bug
  (connector layer), already fixed and verified merged there.
- Don't invent fabricated multi-year milestones in ROADMAP.md — this repo's
  roadmap was explicitly corrected once already for exactly that pattern
  (prior revisions listed shipped features as "future" and had milestones
  through 2031). Keep any edits honest and dated only where real.

## Where to look first (for Fable 5 onboarding)
1. `ROADMAP.md` — corrected, honest status as of 2026-06-27.
2. This file — handoff snapshot as of 2026-07-02.
3. PR #347 — the one open item, review before starting new work.
4. GitHub API/`gh` directly for current issue/PR state — cached memory
   summaries have been observed stale (claiming work "in progress,
   uncommitted" that was already merged days earlier).
