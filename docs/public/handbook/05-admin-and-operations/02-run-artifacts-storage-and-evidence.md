# Managing Run Data and Storage

Transist prioritizes **inspectability** over a tiny storage footprint. This means that every time you run a workflow, Transist saves a significant amount of data to ensure you have a perfect record of what happened.

## Where the Space Goes

As you run workflows, you'll see your Transist Home grow in three main areas:

1. **Artifacts**: The files and reports produced by your steps.
2. **Evidence**: The proof material (logs, API responses) that explains those results.
3. **Executions**: Every attempt at a step. If a step fails and you **Resume** it, Transist saves both the failed attempt and the successful one in separate folders.
4. **Provenance**: Additive `provenance.json` records that summarize trusted workflow/plugin identity, run-start policy outcomes, and denied-run audit details.

This "no-overwrite" policy is essential for auditability, but it does mean that storage usage will accumulate over time.

## Retention and Cleanup

Because the run history is so valuable for debugging and audit, Transist doesn't
automatically delete old runs. Workflow cleanup is separate from history retention:

* **Workflow Cleanup**: Use the `cleanup` block for declared teardown through the
  normal adapter/plugin/action boundary. Every entry matching the root outcome
  (`success`, `canceled`, or `failed`) runs automatically. No match means no action;
  failure never implicitly selects cancellation cleanup. Cleanup does not delete
  the runtime's execution history, artifacts, or evidence.
* **Manual Cleanup**: You can delete old run folders from your `runs/` directory if you are certain you no longer need their history.
* **Failed Runs**: Without matching cleanup, a failed run remains recoverable.
  Once failure cleanup begins, primary retry/resume is blocked, even if cleanup
  fails. Use `runs cleanup` to recover unfinished/failed cleanup and replay for
  new primary work. The primary status stays unchanged; inspect cleanup separately.

## Safety First

Even though artifacts and evidence are "just files," you should treat them with care:

* **No Secrets**: Runtime-known raw values and padded or unpadded standard and URL-safe base64 variants are removed from durable names and bytes. UTF-8 files are redacted; binary files containing a known variant are rejected. This does not cover arbitrary transformations or external exfiltration, so avoid intentionally writing sensitive data into artifacts or logs.
* **Attribution**: Every file in the `runs/` directory is linked to a specific step and execution. If you move these files around manually, you'll break the connection that makes **Replay** and inspection work.

## The Operational Trade-off

The Transist storage model is a trade-off: we use more disk space so that you can have **trust**. By keeping a complete, immutable record of every execution, we ensure that you can always answer the question: "What exactly happened during that run?"

For trusted runs, the shortest path to "what did the runtime verify and why did it allow or deny execution" is `provenance.json` at the run root. Use `state.json` for orchestration state and `provenance.json` for audit-oriented trust and policy metadata.

---

### Related

* [Operating the Transist Home Directory Guide](../../guides/inspection/operating-the-transist-home-directory.md)
* [Determinism, Auditability, and Governance Guide](../../guides/trust/determinism-auditability-and-governance-by-example.md)
* [Storage Layout Reference](../../reference/storage-layout.md)

**Navigation:**

* Previous: [Operating Transist Locally](01-operating-transist-locally.md)
* Next: [Stack and Governance](03-stack-and-governance.md)
