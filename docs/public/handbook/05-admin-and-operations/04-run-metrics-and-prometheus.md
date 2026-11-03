# Run Metrics and Monitoring

Transist provides first-class monitoring tools so you can keep track of how your automation is performing. Whether you want a quick count of failures or a detailed look at AI token costs, Transist makes this data available without requiring a separate database.

## What is tracked?

Transist automatically collects metrics for almost everything that happens:

* **Run History**: Counts of started, finished, failed, and replayed runs.
* **Step Performance**: How long each step takes and how many times it has been retried.
* **Human Decisions**: How long operators take to approve a gate, and how often they approve vs. reject.
* **AI Usage**: Token counts and associated costs (when supported by the plugin).

## Real-time vs. Durable Metrics

Because Transist is local-first, it handles metrics in two ways:

* **Live**: The `transist-server` keeps a cached, in-memory view of your metrics so it can respond instantly to requests.
* **Durable**: All metrics are ultimately derived from the run data on your disk. This means that if you restart the server, it simply "re-reads" the history from your Transist Home to rebuild its view.

## Using Prometheus

The Companion Server exposes a standard Prometheus endpoint at:
`GET /metrics`

You can point a local Prometheus instance at this URL to build dashboards in tools like Grafana.

## Safety and Privacy

We take privacy seriously, even in our metrics:

* **No PII**: Prometheus labels never include sensitive information like Run IDs, Step IDs, or user names.
* **No Secrets**: Your secrets and prompt contents are never exported to the metrics endpoint.
* **Local Only**: By default, the metrics server only listens on your local machine.

## What this is *not*

The metrics feature is designed for single-user or single-instance monitoring. It is **not**:

* A replacement for a billing engine.
* A way to manage multiple Transist instances across a large company.
* An analytics platform for long-term data storage.

It's a power tool for the operator who wants to know exactly what's happening in their local automation factory.

---

### Related

* [Observing Runs with Prometheus Guide](../../guides/inspection/observing-runs-with-prometheus.md)
* [Metrics Reference](../../reference/metrics.md)
* [Companion Server API Reference](../../reference/companion-server-api.md)

**Navigation:**

* Previous: [Stack and Governance](03-stack-and-governance.md)
