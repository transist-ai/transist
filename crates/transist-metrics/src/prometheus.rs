// SPDX-License-Identifier: MPL-2.0
//! Prometheus rendering helpers for cached metrics samples.

use super::*;
use crate::index::{SampleKey, UsageAccumulator, UsageKey};

pub(crate) fn render_prometheus(samples: &BTreeMap<SampleKey, f64>) -> Result<String> {
    let mut out = String::new();
    for family in metric_families() {
        out.push_str("# HELP ");
        out.push_str(family.name);
        out.push(' ');
        out.push_str(family.help);
        out.push('\n');
        out.push_str("# TYPE ");
        out.push_str(family.name);
        out.push(' ');
        out.push_str(family.kind);
        out.push('\n');
        for (key, value) in samples {
            if key.name == family.name
                || (family.kind == "histogram"
                    && (key.name == format!("{}_bucket", family.name)
                        || key.name == format!("{}_sum", family.name)
                        || key.name == format!("{}_count", family.name)))
            {
                out.push_str(&key.name);
                if !key.labels.is_empty() {
                    out.push('{');
                    for (index, (label, raw_value)) in key.labels.iter().enumerate() {
                        if index > 0 {
                            out.push(',');
                        }
                        out.push_str(label);
                        out.push_str("=\"");
                        out.push_str(&escape_label_value(raw_value));
                        out.push('"');
                    }
                    out.push('}');
                }
                out.push(' ');
                out.push_str(&format_sample_value(*value));
                out.push('\n');
            }
        }
    }
    Ok(out)
}

struct MetricFamily {
    name: &'static str,
    kind: &'static str,
    help: &'static str,
}

fn metric_families() -> &'static [MetricFamily] {
    &[
        MetricFamily {
            name: "transist_runs_created_total",
            kind: "counter",
            help: "Total runs created.",
        },
        MetricFamily {
            name: "transist_runs_started_total",
            kind: "counter",
            help: "Total runs started.",
        },
        MetricFamily {
            name: "transist_runs_finished_total",
            kind: "counter",
            help: "Total runs finished.",
        },
        MetricFamily {
            name: "transist_runs_finished_by_status_total",
            kind: "counter",
            help: "Total runs finished by terminal status.",
        },
        MetricFamily {
            name: "transist_runs_replayed_total",
            kind: "counter",
            help: "Total runs replayed.",
        },
        MetricFamily {
            name: "transist_runs_resumed_total",
            kind: "counter",
            help: "Total runs resumed.",
        },
        MetricFamily {
            name: "transist_runs_active",
            kind: "gauge",
            help: "Runs currently active.",
        },
        MetricFamily {
            name: "transist_runs_blocked_on_gates",
            kind: "gauge",
            help: "Runs currently blocked on gates.",
        },
        MetricFamily {
            name: "transist_step_executions_started_total",
            kind: "counter",
            help: "Total step executions started from persisted step-state execution counts.",
        },
        MetricFamily {
            name: "transist_step_executions_finished_total",
            kind: "counter",
            help: "Total step executions finished.",
        },
        MetricFamily {
            name: "transist_step_executions_finished_by_status_total",
            kind: "counter",
            help: "Total step executions finished by terminal status.",
        },
        MetricFamily {
            name: "transist_step_retries_total",
            kind: "counter",
            help: "Total step retries.",
        },
        MetricFamily {
            name: "transist_gate_waits_total",
            kind: "counter",
            help: "Total gate waits entered.",
        },
        MetricFamily {
            name: "transist_gate_approvals_total",
            kind: "counter",
            help: "Total gate approvals.",
        },
        MetricFamily {
            name: "transist_gate_rejections_total",
            kind: "counter",
            help: "Total gate rejections.",
        },
        MetricFamily {
            name: "transist_run_duration_ms",
            kind: "histogram",
            help: "Run duration in milliseconds.",
        },
        MetricFamily {
            name: "transist_step_execution_duration_ms",
            kind: "histogram",
            help: "Step-execution duration in milliseconds.",
        },
        MetricFamily {
            name: "transist_gate_wait_duration_ms",
            kind: "histogram",
            help: "Gate wait duration in milliseconds.",
        },
        MetricFamily {
            name: "transist_usage_input_tokens_total",
            kind: "counter",
            help: "Total input tokens reported by adapters.",
        },
        MetricFamily {
            name: "transist_usage_output_tokens_total",
            kind: "counter",
            help: "Total output tokens reported by adapters.",
        },
        MetricFamily {
            name: "transist_usage_total_tokens_total",
            kind: "counter",
            help: "Total tokens reported by adapters.",
        },
        MetricFamily {
            name: "transist_usage_reports_total",
            kind: "counter",
            help: "Total usage payloads observed, including partial reports.",
        },
        MetricFamily {
            name: "transist_metrics_rebuild_errors_total",
            kind: "counter",
            help: "Total metrics rebuild errors.",
        },
        MetricFamily {
            name: "transist_metrics_export_errors_total",
            kind: "counter",
            help: "Total metrics export errors.",
        },
        MetricFamily {
            name: "transist_metrics_validation_errors_total",
            kind: "counter",
            help: "Total invalid adapter metric entries discarded during durable processing.",
        },
    ]
}

pub(crate) fn histogram_samples(
    samples: &mut BTreeMap<SampleKey, f64>,
    metric_name: &str,
    labels: Vec<(String, String)>,
    value_ms: u64,
    buckets: &[u64],
) {
    for bucket in buckets {
        if value_ms <= *bucket {
            let mut bucket_labels = labels.clone();
            bucket_labels.push(("le".to_string(), bucket.to_string()));
            add_sample(
                samples,
                &format!("{metric_name}_bucket"),
                bucket_labels,
                1.0,
            );
        }
    }
    let mut inf_labels = labels.clone();
    inf_labels.push(("le".to_string(), "+Inf".to_string()));
    add_sample(samples, &format!("{metric_name}_bucket"), inf_labels, 1.0);
    add_sample(
        samples,
        &format!("{metric_name}_sum"),
        labels.clone(),
        value_ms as f64,
    );
    add_sample(samples, &format!("{metric_name}_count"), labels, 1.0);
}

pub(crate) fn add_sample(
    samples: &mut BTreeMap<SampleKey, f64>,
    metric_name: &str,
    labels: Vec<(String, String)>,
    value: f64,
) {
    *samples
        .entry(SampleKey {
            name: metric_name.to_string(),
            labels,
        })
        .or_insert(0.0) += value;
}

pub(crate) fn bounded_labels<const N: usize>(pairs: [(&str, String); N]) -> Vec<(String, String)> {
    let mut labels = pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect::<Vec<_>>();
    labels.sort();
    labels
}

pub(crate) fn step_labels(
    workflow_labels: &[(String, String)],
    step_kind: &str,
    adapter_kind: Option<&str>,
    action: Option<&str>,
) -> Vec<(String, String)> {
    let mut labels = workflow_labels.to_vec();
    labels.push(("step_kind".to_string(), step_kind.to_string()));
    labels.push((
        "adapter_kind".to_string(),
        adapter_kind.unwrap_or_default().to_string(),
    ));
    labels.push(("action".to_string(), action.unwrap_or_default().to_string()));
    labels.sort();
    labels
}

pub(crate) fn usage_labels(
    workflow_labels: &[(String, String)],
    step_kind: &str,
    adapter_kind: Option<&str>,
    action: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
) -> Vec<(String, String)> {
    let mut labels = step_labels(workflow_labels, step_kind, adapter_kind, action);
    labels.push((
        "provider".to_string(),
        provider.unwrap_or_default().to_string(),
    ));
    labels.push(("model".to_string(), model.unwrap_or_default().to_string()));
    labels.sort();
    labels
}

pub(crate) fn usage_breakdown_entries(
    usage_breakdown: BTreeMap<UsageKey, UsageAccumulator>,
) -> Vec<UsageBreakdownEntry> {
    usage_breakdown
        .into_iter()
        .map(|(key, value)| value.into_entry(key))
        .collect()
}

fn format_sample_value(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i128)
    } else {
        format!("{value}")
    }
}

fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}
