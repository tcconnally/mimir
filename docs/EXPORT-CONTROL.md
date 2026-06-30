# Export Control Self-Classification — Perseus & Mimir

**Prepared by:** Perseus Computing LLC
**Date:** 2026-06-20
**Classification:** UNCLASSIFIED
**Jurisdiction:** United States

---

## Executive Summary

Both Perseus (perseus-ctx) and Mimir are **MIT-licensed, publicly available open source software**.
Under the Export Administration Regulations (EAR) 15 CFR §§ 730-774 and the International
Traffic in Arms Regulations (ITAR) 22 CFR §§ 120-130, both products are self-classified as
**EAR99** — not subject to export licensing requirements.

This document provides the analysis and rationale for federal procurement and SBIR submissions.

---

## Product Descriptions

### Perseus (perseus-ctx v1.0.8)
- **Type:** Python CLI tool — live context engine for AI assistants
- **Function:** Resolves directives (@query, @read, @services) into plain markdown output. Read-only — never writes to filesystem, never stores data, never makes network calls unless explicitly directed by user-authored directives.
- **Distribution:** PyPI (public package registry)
- **Source:** https://github.com/Perseus-Computing-LLC/perseus (public, MIT license)

### Mimir (v2.0.0)
- **Type:** Rust binary — persistent memory MCP server for AI agents
- **Function:** Stores, searches, and retrieves structured entities via JSON-RPC 2.0 over stdio. Includes AES-256-GCM encryption (standard commercial algorithm). Optional embedding via public ONNX models. No cryptographic key management — keys are user-provided.
- **Distribution:** GitHub Releases (public binary downloads)
- **Source:** https://github.com/Perseus-Computing-LLC/mneme (public, MIT license)

---

## EAR Analysis

### Jurisdiction: EAR99

Per 15 CFR § 734.3(b), items subject to the EAR include all items in the United States
unless specifically excluded. The following analysis determines whether Perseus or Mimir
fall under a more restrictive ECCN (Export Control Classification Number).

### ECCN Review

| ECCN Category | Applicable? | Rationale |
|---|---|---|
| **3A001** (electronics) | No | No electronic components or hardware |
| **3D001** (software for 3A) | No | Not development/production software for controlled electronics |
| **4A003** (computers) | No | Not a computer or computing system |
| **4D001** (software for 4A) | No | Not operating system or development software for controlled computers |
| **5A002** (cryptography) | Reviewed below | See cryptography analysis |
| **5D002** (cryptographic software) | Reviewed below | See cryptography analysis |
| **9A004** (spacecraft) | No | No aerospace applications |

### Cryptography Analysis (ECCN 5D002)

Mimir includes AES-256-GCM encryption via the `aes-gcm` Rust crate (v0.10). Under
EAR Category 5 Part 2, cryptographic software may require classification under 5D002.

**However**, per 15 CFR § 740.13(b)(1) and Supplement No. 8 to Part 742, "publicly
available" encryption source code is **not subject to the EAR** when it is:

1. Published on a publicly accessible website (GitHub — ✅)
2. Available for free distribution (MIT license — ✅)
3. Not restricted to specific countries or persons (public repo — ✅)

Both Perseus and Mimir source code are publicly available on GitHub under MIT license.
The AES-256-GCM implementation is via a standard, publicly available open-source crate.
No proprietary or classified encryption algorithms are used.

**Conclusion:** EAR99. Encryption component falls under the public availability exclusion.

### De Minimis Analysis

The only non-US component is the `aes-gcm` Rust crate, which is also publicly available
open source. No controlled foreign content above the de minimis threshold (25% for most
countries, 10% for embargoed destinations).

---

## ITAR Analysis

### USML Review

Per 22 CFR § 121.1 (United States Munitions List), ITAR controls apply to defense
articles and services. Neither Perseus nor Mimir:

- Are specifically designed, developed, configured, adapted, or modified for a
  military application (Category I-XXI)
- Contain classified information
- Are listed on the USML

Both products are **general-purpose AI infrastructure tools** with no military-specific
features, no weapons interfaces, no fire control integration, and no classified data handling.

**Conclusion:** Not subject to ITAR. No DDTC registration or export license required.

### ITAR Note for SBIR

Some DoD SBIR topics carry ITAR restrictions because the TOPIC involves defense
articles, not because the offeror's technology is ITAR-controlled. In such cases,
offerors must disclose any use of foreign nationals and comply with topic-level
export control requirements. Perseus Computing LLC is US-owned and US-operated
(all development in the United States, no foreign nationals on the codebase).

---

## OFAC / Sanctions

Neither product is designed for use in, or exported to, comprehensively sanctioned
countries (Cuba, Iran, North Korea, Syria, Crimea region of Ukraine). As public
open source software, standard EAR99 treatment applies.

---

## Summary

| Product | ECCN | ITAR | License Required | Encryption Registration |
|---|---|---|---|---|
| Perseus (perseus-ctx) | EAR99 | Not applicable | No | No |
| Mimir | EAR99 | Not applicable | No | No (public availability exclusion) |

---

## For Federal Procurement

This self-classification supports:

- **SBIR proposal submissions** — provides ITAR/EAR disclosure required by DoD topics
- **CMMC compliance** — confirms no foreign-controlled technology
- **FedRAMP authorization** — supports supply chain risk assessment
- **GSA Schedule qualification** — provides export control posture

**Certification:** I certify that this self-classification has been prepared in good
faith based on a reasonable understanding of the EAR and ITAR. Perseus Computing LLC
is a US-owned small business. All development is performed in the United States by
US persons.

---

*This document is not legal advice. For formal export classification, consult an
export control attorney or submit a Commodity Classification request to BIS.*
