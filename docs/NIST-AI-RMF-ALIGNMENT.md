# NIST AI RMF Alignment — Perseus & Mimir

> Mapping Perseus (live context engine) and Mimir (persistent memory) to the
> NIST AI Risk Management Framework (AI RMF 1.0, January 2023).

**Prepared by:** Perseus Computing LLC
**Date:** 2026-06-20
**Status:** Living document — updated as features ship

---

## Overview

The NIST AI Risk Management Framework defines four core functions: **Govern,
Map, Measure, Manage.** This document maps Perseus and Mimir capabilities to
specific AI RMF subcategories, demonstrating alignment for federal procurement
and ATO (Authority to Operate) submissions.

---

## GOVERN — Establish organizational context for AI risk management

| AI RMF Subcategory | How We Address It |
|---|---|
| **GOVERN 1.1** — Legal/regulatory requirements for AI are understood | MIT license ensures no copyleft risk. SBOMs published per EO 14028. SECURITY.md discloses attack surface and compliance posture. |
| **GOVERN 1.2** — AI system transparency and accountability mechanisms | Perseus traces every context injection to source (file, line, timestamp). Mimir journal provides immutable decision provenance. |
| **GOVERN 1.4** — Organizational risk tolerance for AI is established | Both systems are read-only by default. Perseus never writes to filesystem. Mimir's connectors are opt-in. Explicit trust boundaries documented. |
| **GOVERN 2.1** — AI system design aligns with organizational values | MIT-licensed, no vendor lock-in. US-owned small business. All development in US. |
| **GOVERN 2.3** — AI system inventory maintained and accessible | Mimir's entity model provides typed, searchable inventory of all knowledge assets. Perseus's context snapshots serve as an operational inventory. |
| **GOVERN 3.1** — Decision-making accountability assigned | Perseus's decision trace maps agent actions to specific context state. Mimir's journal records who touched what and when. |
| **GOVERN 4.1** — Organizational practices for AI risk management documented | This document. Plus SECURITY.md, SBOM, and federal buyers guide for each repo. |
| **GOVERN 5.1** — AI risk management integrated with cybersecurity | AES-256-GCM encryption (Mimir). Zero network by default (both). CMMC Level 2 readiness (Mimir). Attack surface analysis documented. |
| **GOVERN 6.1** — Third-party AI risks managed | SBOMs audit every dependency. All dependencies MIT/Apache-2.0 — no copyleft. Supply chain attestation in progress (SLSA). |

**Status:** 🟡 Map domain largely covered. Gaps: formal risk appetite statement (GOVERN 1.4), third-party risk assessment process (GOVERN 6.1).

---

## MAP — Establish context to frame AI risks

| AI RMF Subcategory | How We Address It |
|---|---|
| **MAP 1.1** — Intended purpose and deployment context understood | Perseus is a read-only context engine — explicitly not a decision-maker. Mimir stores agent memory — explicitly not a model. Deployment context: local-first, air-gap ready, MCP-native. |
| **MAP 1.2** — AI system's tasks and expected behavior defined | Perseus: resolve directives → render markdown → inject. Mimir: store/recall/search entities → respond via MCP JSON-RPC. Both documented with input/output contracts. |
| **MAP 1.3** — AI system's knowledge limits documented | Perseus renders only what directives resolve — no hallucination. Mimir searches only stored entities — no generative capacity. Both systems are "dumb infrastructure" — they provide ground truth, not opinions. |
| **MAP 1.4** — AI system's outputs and decisions characterized | Perseus output is deterministic markdown. Mimir output is structured JSON. Both are versioned, reproducible, and auditable. |
| **MAP 2.1** — Benefits and potential negative impacts assessed | Benefits: reduces LLM hallucination (context grounding), enables institutional memory (persistent entities). Negative impacts: improper use could surface stale or incorrect data if not refreshed. Mitigated by context TTL and decay scoring. |
| **MAP 2.2** — AI system evaluated for trustworthiness characteristics | Safe (read-only, no side effects), Secure (AES-256-GCM, zero network), Fair (deterministic output), Interpretable (full provenance), Privacy-enhanced (local-first, no telemetry). |
| **MAP 3.1** — Environment and deployment conditions mapped | Both systems run on Linux/macOS/Windows. Air-gapped mode works without internet. Container deployment documented. Classified environment deployment in roadmap. |
| **MAP 3.2** — Human-AI configuration and oversight delineated | Perseus: human authors directives, engine resolves them, assistant reads output. Mimir: human configures encryption, agent uses memory, audit trail is human-readable. Clear boundaries at every interface. |
| **MAP 4.1** — Approaches to measuring trustworthiness identified | Perseus: context freshness (TTL), directive resolution time, token compression ratio. Mimir: entity recall precision, journal integrity (hash chain), decay scoring. Gauntlet v2 benchmark: 100.0/100 score. |

**Status:** 🟢 Map function well-covered. Strong because both products are deterministic infrastructure, not black-box AI.

---

## MEASURE — Assess AI risk using quantitative and qualitative methods

| AI RMF Subcategory | How We Address It |
|---|---|
| **MEASURE 1.1** — Trustworthiness characteristics tested | Perseus: 1,032 tests in CI. Mimir: Rust test suite + demo smoke tests. Both: deterministic output testing. |
| **MEASURE 1.2** — System performance, safety, security assessed | Perseus: 450x cold→warm speedup, 94% token compression, 0 failures at 150 concurrent writes. Mimir: sub-ms SQLite queries, FTS5 + hybrid vector search. Security: attack surface analysis in SECURITY.md. |
| **MEASURE 2.1** — AI system functionality and behavior monitored | Perseus context snapshots provide continuous monitoring of operational state. Mimir journal provides append-only operation log. Both: CI on every push. |
| **MEASURE 2.2** — AI system regularly evaluated against trustworthiness requirements | Gauntlet v2 benchmark run on releases. CI suite covers regression. Roadmap includes continuous monitoring plan (Phase 4). |
| **MEASURE 2.3** — AI system evaluated for safety concerns | Read-only context engine — no safety risk from autonomous action. Memory engine — encryption prevents data exposure. Both: no model inference, no autonomous behavior. |
| **MEASURE 2.4** — Security and resiliency assessed | SECURITY.md published for both. AES-256-GCM encryption verified. Dependency auditing (pip-audit, cargo-audit). Attack surface analyzed. |
| **MEASURE 2.5** — AI system evaluated for privacy risks | Local-first: no data leaves the deployment boundary. No telemetry. No analytics. No cloud dependency. Memory encryption at rest. Workspace isolation in Mimir. |
| **MEASURE 2.6** — Fairness and bias evaluated | Deterministic context rendering — no model bias. Structured entity storage — no training data bias. Both are data infrastructure, not decision systems. |
| **MEASURE 2.7** — Explainability, interpretability, and transparency evaluated | Perseus: full context provenance (file, line, timestamp). Mimir: typed entities with relationship graph and journal trail. PROV-O export (in development). Decision trace dashboard (in development). |
| **MEASURE 2.8** — AI system evaluated for environmental impact | Perseus: single Python process, no GPU. Mimir: single Rust binary, 10MB footprint, bundled SQLite. Both: minimal compute requirements. |
| **MEASURE 3.1** — Measurement results documented and communicated | This document. SBOMs published. SECURITY.md published. Gauntlet v2 results public. CI badges on repos. |

**Status:** 🟡 Well-covered for current maturity. Gaps: formal third-party security assessment (Phase 4), continuous monitoring automation (Phase 4).

---

## MANAGE — Respond to AI risks and maximize benefits

| AI RMF Subcategory | How We Address It |
|---|---|
| **MANAGE 1.1** — AI risks prioritized and responded to | Public issue trackers for both repos. Security vulnerability reporting process (perseus@perseus.observer, 48-hour response). Correction capture (Mimir: mimir_correct tool). |
| **MANAGE 1.2** — AI risks treated, transferred, avoided, or accepted | MIT license: risk transferred to deployer (standard OSS model). SECURITY.md: risks explicitly disclosed. Trust boundaries documented. |
| **MANAGE 1.3** — Benefits maximized, negative impacts minimized | Context grounding (Perseus) minimizes hallucination risk. Persistent memory (Mimir) enables institutional knowledge. Both are BYO-model — no lock-in to specific LLM. |
| **MANAGE 2.1** — AI system usage, impacts, and incidents documented | Mimir journal provides append-only, tamper-evident operation log. Perseus context snapshots document operational state. Error reporting in CI. |
| **MANAGE 2.2** — AI system decommissioned per policy | Not applicable (both are pure infrastructure — no persistent state beyond user's database/context files). Mimir database is single-file SQLite for easy archival. |
| **MANAGE 2.3** — Post-deployment monitoring and improvement | Roadmap includes: continuous monitoring (Phase 4), incident response plan (Phase 4), SLSA attestation (Phase 1), FedRAMP path (Phase 4). |
| **MANAGE 3.1** — Communication with relevant AI actors | Maintained: issue trackers, CONTRIBUTING.md, documentation site. In development: security advisory process, CHANGELOG, release notes. |
| **MANAGE 4.1** — AI risk management documentation maintained | This document is living. Updated when features ship. Version controlled alongside code. |

**Status:** 🟡 Manage function partially covered. Gaps: incident response plan, decommissioning procedure, formal risk acceptance documentation.

---

## Summary

| Function | Coverage | Key Gaps |
|---|---|---|
| **GOVERN** | 🟡 70% | Formal risk appetite statement, third-party risk process |
| **MAP** | 🟢 90% | None significant — deterministic infrastructure maps naturally |
| **MEASURE** | 🟡 75% | Third-party security assessment, continuous monitoring |
| **MANAGE** | 🟡 60% | Incident response plan, FedRAMP SSP, formal risk acceptance |

**Overall alignment:** 🟡 ~74% — strong foundation for Phase 1/2. Full alignment targeted for Phase 4 (FedRAMP path).

---

## References

- NIST AI RMF 1.0: https://www.nist.gov/itl/ai-risk-management-framework
- NIST AI RMF Playbook: https://airc.nist.gov/AI_RMF_Knowledge_Base/Playbook
- NIST SP 800-53 Rev 5: Security and Privacy Controls
- NIST SP 800-207: Zero Trust Architecture
- EO 14028: Improving the Nation's Cybersecurity (SBOM requirement)
