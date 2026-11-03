# Documentation Style Guide

This guide defines the writing, structure, and terminology rules for all Transist documentation. Following these rules keeps the docs consistent, professional, and technically precise.

---

## Our Tone & Voice

We write for five primary audiences:

1. **Technical evaluators**: People installing Transist, running a workflow, and inspecting the result during evaluation.
2. **Workflow authors**: People building repeatable AI/tool workflows.
3. **Operators and platform engineers**: People responsible for local or self-hosted operation, inspection, storage, metrics, and recovery.
4. **Extension authors and maintainers**: People building plugins, adapters, or source-level integrations under an approved channel.
5. **Technical decision-makers**: Team leads, staff engineers, senior product managers, and engineering leaders deciding where Transist fits in a team stack.

**Our voice is**:

- **Human-Friendly**: We use natural language and clear analogies.
- **Direct**: We don't use filler phrases like "essentially" or "basically."
- **Professional**: We are serious about reliability and safety.
- **Implementation-First**: We describe what the code *does*, not what we hope it will do someday.

Write to the reader, not to an AI agent executing a checklist. Requirements should disappear into useful documentation.

---

## Terminology Rules

Consistency is key to understanding. Always use the **Preferred Term**.

| Concept | Preferred Term | Avoid | Why? |
| :--- | :--- | :--- | :--- |
| Automation definition | **Workflow** | Flow file, Job, Pipeline | "Workflow" is the user-facing mental model. |
| Execution instance | **Run** | Execution, Job run | "Run" is the primary noun for a live workflow. |
| Central runtime object | **Workflow run** | Agent session | The run owns state, evidence, and recovery. |
| Run identifier | **Run ID** | Run ref, Execution ID | Keeps inspection and CLI commands consistent. |
| Unit inside workflow | **Step** | Task node, Action item | "Step" is the standard YAML key. |
| One attempt of a step | **Execution** | Retry | We record every execution separately for the audit trail. |
| Manual pause | **Gate** | Approval, Checkpoint | "Gate" is the formal control primitive. |
| Primary output | **Artifact** | Output file | Artifacts are part of the permanent record. |
| Audit material | **Evidence** | Trace, Log file | Evidence is for "showing your work." |
| External tool | **Execution participant** | Worker, agent owner | Agents and tools perform work; Transist governs the run. |
| Home directory | **Transist home** | Data dir, Home dir, global root | The per-user default (`transist home init` with no other override) is the most common choice; `transist home set`, `TRANSIST_HOME`, and `--home` can choose another path. |
| User-persisted home | **user-set home** | default home, global root | The path saved with `transist home set`; used flat when no higher-priority source resolves. |
| Snapshot of flow | **Recipe** | Snapshot, Draft | A Recipe is the immutable version used for a Run. |

---

## Writing Rules

- **Concepts First**: Explain *why* something exists before diving into *how* it works.
- **Separate Styles**: Don't mix tutorials (Handbook) with technical specs (Reference).
- **Be Explicit**: Clearly state what the system guarantees and what it does not.
- **No Speculation**: Only document features that exist in the current version.
- **Use Examples**: A single YAML snippet is often better than three paragraphs of text.
- **Condense Bullets**: Prefer terse, imperative bullets over long, narrative paragraphs.
- **Name the Reader**: Make the intended reader clear when a page is not general-purpose.
- **Use Accurate Distribution Terms**: Where the distribution model matters, say evaluation, licensed, approved channel, or customer-owned. Do not imply open-source or community-first distribution where that is not the case.
- **Avoid Buyer-Only Copy in Technical Docs**: Keep product value concrete, but prefer operational facts over sales language.

---

## Structure & Navigation

### Handbook (docs/public/handbook/)

- **Purpose**: Concept-oriented teaching.
- **Structure**: Purpose -> Concept -> Practical Usage -> Related Links.
- **Navigation**: Must include "Previous" and "Next" links to guide the reader.

### Guides (docs/public/guides/)

- **Purpose**: Task-oriented walkthroughs.
- **Structure**: Goal -> Prerequisites -> Steps -> Verification.
- **Style**: Must be runnable and verified against the current version.

### Reference (docs/public/reference/)

- **Purpose**: Field-by-field and endpoint-by-endpoint technical specs.
- **Style**: Concise, exact, and free of "storytelling."

---

## Cross-Linking

- Link **sideways** (Handbook to Handbook) only when a concept is a prerequisite.
- Link **downward** (Handbook/Guide to Reference) for technical lookups.
- Link **outward** (Handbook to Guide) to show how to use a concept in practice.

---

## Review Checklist

Before you submit a documentation change, ask yourself:

1. Is the terminology consistent with the table above?
2. Does this concept have one canonical home, or am I duplicating text?
3. Have I removed or updated any contradictory or stale information?
4. Is the tone appropriate for the intended persona?
5. Does the page avoid unintended licensing or support commitments unless explicitly stated?
6. Does the page separate customer-facing guarantees from maintainer-only notes?
