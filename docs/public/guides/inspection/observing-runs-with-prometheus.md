# Guide: Observing Runs with Prometheus

This guide is for operators and platform engineers who need aggregate runtime signals from `transist-server`. The Prometheus-compatible metrics endpoint can track run counts, execution timing, gate waits, and usage signals reported by plugins.

## The Metrics Endpoint

The companion server includes a dedicated `/metrics` endpoint. This is a standard scrape target that any Prometheus-compatible monitoring tool can read.

### 1. Start the Companion Server

First, ensure the server is running in your Transist home directory.

```bash
transist-server --port 7777
```

### 2. View the Raw Metrics

You can see the raw Prometheus data by simply visiting the metrics URL or using `curl`.

```bash
curl -s http://127.0.0.1:7777/metrics
```

## What’s Inside?

When you look at the metrics output, you’ll see several "families" of data. Here are the most important ones to watch:

- **`transist_runs_created_total`**: A simple counter that tracks how many workflows have been run.
- **`transist_step_executions_started_total`**: How many individual steps have been attempted.
- **`transist_step_execution_duration_ms`**: A histogram showing how long steps are taking to complete. Use it to identify performance bottlenecks.
- **`transist_gate_wait_duration_ms`**: A histogram showing how long manual gates wait before a decision is recorded.
- **`transist_usage_total_tokens_total`**: If you're using AI-powered plugins, this counter tracks the total number of tokens reported by the adapter or plugin, helping you monitor usage.
- **`transist_runs_finished_total`**: Tracks how many runs have reached a terminal state (success, failure, or cancelled).

### Timer Taxonomy (Important)

When interpreting metrics and inspection output, use this model:

- **Wall clock**: lifecycle elapsed time for the run.
- **Active execution**: summed tool/runtime work across step attempts.
- **Blocked wait**: summed manual-gate wait time.

`total_duration_ms` remains a wall-clock value for compatibility.

## Key Concepts

- **Bounded Labels:** To keep the monitoring system performant, Transist uses "bounded labels." This means we don't include infinitely growing data like `run_id` or `step_id` as labels in the metrics.
- **Usage Reporting:** Usage counters (like tokens) are updated whenever a plugin reports usage data. Even if a run fails, any tokens consumed up to that point are recorded.
- **Real-time Visibility:** The metrics reflect the current state of your local Transist environment. As soon as a step finishes, the duration is added to the histogram.

## When to Use Metrics vs. Inspection

- **Use Metrics** when you need aggregate operational signals such as workflow counts, failure rates, and step duration trends.
- **Use Inspection** (CLI or UI) when you need to "zoom in" on a specific run. It's the best tool for answering: "Why did this specific run fail?" or "What was the exact output of this script?"

## Next Steps

- [**Companion HTTP Server Guide**](companion-http-server-in-practice.md): Learn more about the other features of the `transist-server`.
- [**Metrics Reference**](../../reference/metrics.md): A complete list of every metric exported by Transist and what they mean.
