# Perseus & Mimir — AI Infrastructure for Government

## The Stack

| Component | What it does | License | Language |
|---|---|---|---|
| **Perseus** | Live context engine — grounds AI agents with verifiable state before every turn | MIT | Python 3.10+ |
| **Mimir** | Persistent memory — encrypted, auditable, FTS5-searchable knowledge store for AI agents | MIT | Rust |

Both are open source, production-deployed, and maintained by Perseus Computing LLC (US-owned).

---

## Why Government Buyers Choose Open Source

- **No vendor lock-in.** MIT license means your agency owns the deployment forever. Switch integrators without switching tools.
- **Supply chain transparency.** Full SBOMs published. Every dependency auditable.
- **Air-gap ready.** Zero cloud dependencies. Deploy in SCIFs and classified environments.
- **Audit native.** Every AI decision traceable to source. Chain-of-custody journal with cryptographic verification.

---

## Compliance at a Glance

| Requirement | Status |
|---|---|
| SBOM (NTIA Minimum Elements) | ✅ Published for both repos |
| License (MIT) | ✅ No copyleft, no GPL/AGPL |
| Encryption at rest | ✅ AES-256-GCM (Mimir) |
| NIST AI RMF alignment | 🟡 In progress |
| FedRAMP path | 🟡 Gap analysis phase |
| Section 508 / accessibility | 🟡 Audit planned |
| Supply chain (SLSA) | 🟡 Attestation in development |

---

## Security

- **Mimir:** AES-256-GCM encryption for all stored entities. Encryption keys never leave the deployment boundary.
- **Perseus:** Context injection is read-only — Perseus never writes to your systems. It renders, injects, and exits.
- **Both:** No telemetry. No phoning home. No usage tracking. Network calls are strictly opt-in (MCP servers you configure).

---

## Deployment Models

### Air-Gapped / Classified
Single-container deployment. All dependencies bundled. No internet required. Suitable for DoD IL5+, IC Directive 503 environments.

### On-Premises
Deploy on agency infrastructure. Full data sovereignty. Integrate with existing identity providers.

### Cloud (Coming)
AWS GovCloud, Azure Government, GCP Assured Workloads. (Roadmap item — contact us for timeline.)

---

## SBIR / RFP Alignment

Perseus and Mimir address multiple government AI priorities:

| Priority Area | How We Address It |
|---|---|
| AI Interpretability | Perseus traces every context injection to source (file, line, timestamp) |
| AI Control | Live context grounding prevents hallucination — agent decisions are anchored to verifiable state |
| Adversarial Robustness | Mimir's cryptographic journal detects tampering. Perseus's context chain is immutable |
| Audit & Compliance | PROV-O provenance exports. Immutable journal with SHA-256 chain-of-custody |
| Knowledge Management | Mimir's FTS5 search + entity graph for cross-session institutional knowledge |

---

## Active Federal Engagements

- **DARPA AI Forge RFI** (DARPA-SN-26-80) — Response submitted June 2026. University partnerships in progress.
- **DoD SBIR/STTR monitoring** — Active pipeline for AI/autonomy topics
- **NSF SBIR** — Targeting "Knowledge and Data Management Technologies" sub-topic

---

## Procurement Information

| Field | Value |
|---|---|
| Entity | Perseus Computing LLC |
| UEI | [Pending SAM.gov registration] |
| CAGE Code | [Pending] |
| NAICS Codes | 541715 (Primary), 541511, 541512 |
| SBIR Registry | [Pending] |
| Website | https://perseus.observer |
| Contact | perseus@perseus.observer |
| GitHub | https://github.com/Perseus-Computing-LLC |

---

## Get Started

**Evaluate in 5 minutes:**

```bash
# Perseus — context engine
pip install perseus-ctx
perseus --help

# Mimir — persistent memory (MCP server)
# Download binary from https://github.com/Perseus-Computing-LLC/mneme/releases
./mimir --help
```

**For procurement inquiries, security assessments, or ATO support:**
Email perseus@perseus.observer.

---

*Perseus Computing LLC is a US-owned small business. All software is MIT-licensed open source. No proprietary dependencies. No vendor lock-in. No telemetry.*
