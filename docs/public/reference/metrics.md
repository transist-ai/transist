# Metrics and Usage Reference

Transist provides detailed operational metrics and usage signals to help teams understand the behavior, cost, and reliability of their workflows.

## The Transist Metric Model

We distinguish between three classes of metric values to ensure you always know the source and reliability of the data you are viewing.

### 1. Observed Metrics

These are captured directly by the Transist kernel from runtime execution. They are the most trustworthy facts about a run.

- **Examples**: Run duration, step status, retry count, artifact count, gate approval activity.

### 2. Reported Metrics

These are provided by plugins, adapters, or external provider APIs. Transist preserves this metadata and attributes it to the run.

- **Examples**: Provider-reported input/output tokens, external request IDs, model names.

### 3. Unavailable Metrics

These represent usage signals that were expected but could not be captured (e.g., a plugin that does not expose usage data). Transist records `input_tokens`, `output_tokens`, and `total_tokens` only when an adapter or plugin reports exact values.

## Cost Scope

Transist does not perform cost estimation and ships no tokenization-based cost or usage-confidence model. It records token usage only when the adapter or plugin reports an exact value, and it never invents or estimates usage or cost figures. Use reported usage for observability and attribution; it is not a billing reconciliation guarantee.

## Operational Monitoring

Transist exposes these metrics in Prometheus text format at the `GET /metrics` endpoint on the companion server. This allows you to monitor:

- **Run Health**: Success/failure rates by workflow and version.
- **Efficiency**: Where workflows are spending time or hitting retries.
- **Volume**: Tool invocation and artifact generation trends.
- **Wait Times**: How long runs are blocked on manual gate approvals.

## What `/metrics` Does Not Try To Encode

Trust state, plugin/workflow digests, signer key ids, provider locality,
external-transfer evidence, and evidence-export locations are inspection
views, not Prometheus metrics. Use the read-only CLI, `GET /api/v1/runs/...`,
or `/inspect/...` for those facts.

## Timer Semantics

- **Wall clock**: Total time from run start to finish (includes blocked time).
- **Active execution**: The sum of time spent performing actual work in steps.
- **Blocked wait**: The time a run spent waiting for human intervention at a gate.

---

### Related

- [Inspecting Runs, Artifacts, and Evidence](../handbook/03-inspection-and-recovery/02-artifacts-evidence-and-events.md)
- [Companion Server API](companion-server-api.md)
- [Prometheus Integration Guide](../guides/inspection/observing-runs-with-prometheus.md)
