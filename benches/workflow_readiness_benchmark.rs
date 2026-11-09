// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use transist::{
    PreflightMeasurement, PreflightMeasurementPhase, PreflightMeasurementScope,
    RuntimeOperatingMode, TransistContext, capture_preflight_measurements,
};
use transist_protocol::{AdapterKind, Recipe, StepExecutionDescriptor, WorkspaceAccessMode};

const WORKFLOW_REF: &str = "preflight_benchmark@v1";
const RUN_ID: &str = "preflight-benchmark-run";
const ROOT_INVOKES: usize = 13;
const CHILD_INVOKES: usize = 12;
const GRANDCHILD_INVOKES: usize = 12;
const REACHABLE_INVOKES: usize = 37;
const DISTINCT_ACTIONS: usize = 16;
const REPEATED_ACTIONS: usize = REACHABLE_INVOKES - DISTINCT_ACTIONS;
const PROCESS_INVOKES: usize = 30;
const MCP_INVOKES: usize = 7;
const APPLICATION_PROBE_GROUPS: usize = 29;
const APPLICATION_PROCESS_PROBE_GROUPS: usize = 24;
const APPLICATION_MCP_PROBE_GROUPS: usize = 5;
const PLUGIN_PACKAGES: usize = 5;
const DETACHED_ASSETS: usize = 13;
const DETACHED_ASSET_BYTES: usize = 1024;
const DETACHED_ASSET_TOTAL_BYTES: usize = DETACHED_ASSETS * DETACHED_ASSET_BYTES;
const SUBFLOW_DEPTH: usize = 2;

#[derive(Clone, Copy, Debug)]
enum IsolationClass {
    SandboxIsolated,
    AuthorityBearing,
}

impl IsolationClass {
    fn label(self) -> &'static str {
        match self {
            Self::SandboxIsolated => "sandbox_isolated",
            Self::AuthorityBearing => "authority_bearing",
        }
    }

    fn workspace_access(self) -> &'static str {
        match self {
            Self::SandboxIsolated => "read_only",
            Self::AuthorityBearing => "full",
        }
    }
}

struct BenchmarkFixture {
    root: TempDir,
    home: PathBuf,
    context: TransistContext,
}

impl BenchmarkFixture {
    fn new(isolation: IsolationClass) -> Self {
        let root = tempfile::tempdir().expect("create benchmark root");
        let canonical_root = fs::canonicalize(root.path()).expect("canonical benchmark root");
        let home = canonical_root.join("home");
        let workspace = canonical_root.join("workspace");
        let source = canonical_root.join("source");
        fs::create_dir_all(&workspace).expect("create benchmark workspace");
        fs::create_dir_all(source.join("modules")).expect("create module directory");
        fs::create_dir_all(source.join("assets")).expect("create asset directory");
        transist::init_home_dir_with_mode(&home, RuntimeOperatingMode::Standalone)
            .expect("initialize benchmark home");
        let context = TransistContext::with_home(&workspace, &home);
        write_plugins(&canonical_root, &context);
        write_assets(&source);
        write_workflows(&source, &workspace, isolation);
        let recipe_path = context
            .compile_flow_file(&source.join("workflow.yaml"))
            .expect("compile generated benchmark workflow");
        let recipe: Recipe = yaml_serde::from_str(
            &fs::read_to_string(recipe_path).expect("read generated benchmark recipe"),
        )
        .expect("parse generated benchmark recipe");
        assert_topology(&recipe, isolation);

        Self {
            root,
            home,
            context,
        }
    }

    fn assert_read_only_operation(&self) {
        assert!(
            self.context
                .runs_list()
                .expect("list benchmark runs")
                .is_empty(),
            "readiness must not create a run locator"
        );
        let runs = self.home.join("runs");
        assert_eq!(
            fs::read_dir(&runs).expect("read runs directory").count(),
            0,
            "readiness must not create a run directory or persisted event"
        );
    }

    fn finish_run_birth(self) {
        assert_eq!(
            self.context.runs_list().expect("list benchmark runs"),
            vec![RUN_ID.to_string()],
            "each measured run-birth fixture must contain exactly one complete run"
        );
        let run_root = self
            .context
            .run_root(RUN_ID)
            .expect("resolve benchmark run");
        for relative in [
            "state.json",
            "recipe.yaml",
            "context/run_inputs.json",
            "provenance.json",
        ] {
            assert!(run_root.join(relative).is_file(), "missing {relative}");
        }
        let root_path = self.root.path().to_path_buf();
        self.root.close().expect("remove benchmark temporary home");
        assert!(
            !root_path.exists(),
            "benchmark temporary home was not removed"
        );
    }
}

fn write_plugins(root: &Path, context: &TransistContext) {
    for plugin_index in 0..PLUGIN_PACKAGES {
        let adapter = if plugin_index == PLUGIN_PACKAGES - 1 {
            AdapterKind::Mcp
        } else {
            AdapterKind::Process
        };
        let plugin_dir = root.join(format!("plugin-{plugin_index}"));
        fs::create_dir_all(plugin_dir.join("schemas")).expect("create plugin schemas");
        fs::write(plugin_dir.join("schemas/input.json"), "{}\n").expect("write input schema");
        fs::write(plugin_dir.join("schemas/output.json"), "{}\n").expect("write output schema");

        let action_names = (0..DISTINCT_ACTIONS)
            .filter(|action| action % PLUGIN_PACKAGES == plugin_index)
            .map(|action| format!("action_{action:02}"))
            .collect::<Vec<_>>();
        let script = if adapter == AdapterKind::Mcp {
            mcp_script(&action_names)
        } else {
            process_script(plugin_index, &action_names)
        };
        fs::write(plugin_dir.join("plugin.py"), script).expect("write plugin script");
        make_executable(&plugin_dir.join("plugin.py"));
        fs::write(
            plugin_dir.join("plugin.yaml"),
            plugin_manifest(plugin_index, adapter, &action_names),
        )
        .expect("write plugin manifest");
        context
            .install_plugin_file(&plugin_dir.join("plugin.yaml"))
            .expect("install generated benchmark plugin");
    }
}

fn process_script(plugin_index: usize, actions: &[String]) -> String {
    format!(
        r#"#!/usr/bin/env python3
import json
import sys
import time

sys.stdin.readline()
time.sleep(0.05)
print(json.dumps({{
    "type": "app.handshake.response.v1",
    "protocol_version": "app.v1",
    "plugin": {{"id": "benchmark.p{plugin_index}", "version": "1.0.0"}},
    "adapter_kind": "process",
    "capabilities": {{
        "actions": {actions:?},
        "supports_cancel": False,
        "supports_health": False,
        "supports_artifact_hints": False,
        "supports_evidence_hints": False
    }}
}}), flush=True)
"#
    )
}

fn mcp_script(actions: &[String]) -> String {
    let tools = actions
        .iter()
        .map(|action| format!(r#"{{"name":"{action}","inputSchema":{{}}}}"#))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"#!/usr/bin/env python3
import json
import sys
import time

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        result = {{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"benchmark","version":"1"}}}}
    elif method == "tools/list":
        time.sleep(0.05)
        result = {{"tools":[{tools}]}}
    else:
        continue
    print(json.dumps({{"jsonrpc":"2.0","id":request["id"],"result":result}}), flush=True)
"#
    )
}

fn plugin_manifest(plugin_index: usize, adapter: AdapterKind, actions: &[String]) -> String {
    let id = if adapter == AdapterKind::Mcp {
        "benchmark.mcp".to_string()
    } else {
        format!("benchmark.p{plugin_index}")
    };
    let mut manifest = format!(
        "schema: transist.plugin.v1\nid: {id}\nversion: 1.0.0\nadapter: {adapter}\ndefault_provider_kind: local\nentrypoint:\n  type: {adapter}\n  command: plugin.py\npackage:\n  files: [plugin.py, schemas/input.json, schemas/output.json]\nactions:\n"
    );
    for action in actions {
        manifest.push_str(&format!(
            "  - name: {action}\n    input_schema: schemas/input.json\n    output_schema: schemas/output.json\n    secret_inputs: []\n    bindings:\n      transist_prompt_v1:\n        inline_field: prompt\n        file_field: prompt_file\n"
        ));
        if adapter == AdapterKind::Mcp {
            manifest.push_str(&format!("      tool_name: {action}\n"));
        }
    }
    manifest.push_str(
        "capabilities:\n  supports_cancel: false\n  supports_health: false\n  supports_artifact_hints: false\n  supports_evidence_hints: false\n",
    );
    manifest
}

fn write_assets(source: &Path) {
    for index in 0..DETACHED_ASSETS {
        let marker = format!("asset-{index:02}\n");
        let mut contents = marker.repeat(DETACHED_ASSET_BYTES / marker.len() + 1);
        contents.truncate(DETACHED_ASSET_BYTES);
        fs::write(
            source.join(format!("assets/prompt-{index:02}.txt")),
            contents,
        )
        .expect("write detached asset");
    }
}

fn write_workflows(source: &Path, workspace: &Path, isolation: IsolationClass) {
    let grandchild = module_yaml(
        "benchmark_grandchild",
        CHILD_INVOKES + ROOT_INVOKES,
        GRANDCHILD_INVOKES,
        None,
    );
    fs::write(source.join("modules/grandchild.yaml"), grandchild).expect("write grandchild");
    let child = module_yaml(
        "benchmark_child",
        ROOT_INVOKES,
        CHILD_INVOKES,
        Some("grandchild"),
    );
    fs::write(source.join("modules/child.yaml"), child).expect("write child");

    let mut root =
        String::from("schema: transist.flow.v1\nid: preflight_benchmark\nversion: v1\nplugins:\n");
    for plugin_index in 0..PLUGIN_PACKAGES - 1 {
        root.push_str(&format!("  benchmark.p{plugin_index}: 1.0.0\n"));
    }
    root.push_str("  benchmark.mcp: 1.0.0\nmodules:\n  child:\n    path: modules/child.yaml\n");
    root.push_str(&format!(
        "workspace:\n  path: {}\n  access: {}\nnetwork:\n  mode: none\nsteps:\n",
        workspace.display(),
        isolation.workspace_access()
    ));
    for index in 0..ROOT_INVOKES {
        root.push_str(&invoke_yaml(index, Some(index)));
    }
    root.push_str("  - id: child\n    module: child\n");
    fs::write(source.join("workflow.yaml"), root).expect("write root workflow");
}

fn module_yaml(id: &str, first_index: usize, count: usize, child: Option<&str>) -> String {
    let mut yaml = format!("schema: transist.module.v1\nid: {id}\nversion: v1\n");
    if let Some(child) = child {
        yaml.push_str(&format!("modules:\n  {child}:\n    path: {child}.yaml\n"));
    }
    yaml.push_str("steps:\n");
    for index in first_index..first_index + count {
        yaml.push_str(&invoke_yaml(index, None));
    }
    if let Some(child) = child {
        yaml.push_str(&format!("  - id: {child}\n    module: {child}\n"));
    }
    yaml
}

fn invoke_yaml(index: usize, asset: Option<usize>) -> String {
    let action_index = index % DISTINCT_ACTIONS;
    let plugin_index = action_index % PLUGIN_PACKAGES;
    let (adapter, plugin) = if plugin_index == PLUGIN_PACKAGES - 1 {
        ("mcp", "benchmark.mcp".to_string())
    } else {
        ("process", format!("benchmark.p{plugin_index}"))
    };
    let mut yaml = format!(
        "  - id: invoke_{index:02}\n    uses: {adapter}:{plugin}/action_{action_index:02}\n"
    );
    if let Some(asset) = asset {
        yaml.push_str(&format!("    prompt_file: assets/prompt-{asset:02}.txt\n"));
    }
    yaml
}

fn assert_topology(recipe: &Recipe, isolation: IsolationClass) {
    let mut recipes = 0;
    let mut invokes = Vec::new();
    let mut invokes_by_depth = BTreeMap::new();
    let mut max_depth = 0;
    collect_topology(
        recipe,
        0,
        &mut recipes,
        &mut invokes,
        &mut invokes_by_depth,
        &mut max_depth,
    );
    let actions = invokes
        .iter()
        .map(|(_, _, action)| action.as_str())
        .collect::<BTreeSet<_>>();
    let process = invokes
        .iter()
        .filter(|(adapter, _, _)| *adapter == AdapterKind::Process)
        .count();
    let mcp = invokes
        .iter()
        .filter(|(adapter, _, _)| *adapter == AdapterKind::Mcp)
        .count();
    let plugins = invokes
        .iter()
        .map(|(_, plugin, _)| plugin.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(recipes, 3, "exact root and child recipe count");
    assert_eq!(recipe.steps.len(), ROOT_INVOKES + 1, "exact root topology");
    assert_eq!(
        invokes_by_depth,
        BTreeMap::from([
            (0, ROOT_INVOKES),
            (1, CHILD_INVOKES),
            (2, GRANDCHILD_INVOKES),
        ]),
        "exact invokes per recursive recipe"
    );
    assert_eq!(invokes.len(), REACHABLE_INVOKES, "exact reachable invokes");
    assert_eq!(actions.len(), DISTINCT_ACTIONS, "exact distinct actions");
    assert_eq!(
        invokes.len() - actions.len(),
        REPEATED_ACTIONS,
        "exact repeated actions"
    );
    assert_eq!(process, PROCESS_INVOKES, "exact process adapter count");
    assert_eq!(mcp, MCP_INVOKES, "exact MCP adapter count");
    assert_eq!(plugins.len(), PLUGIN_PACKAGES, "exact plugin package count");
    assert_eq!(max_depth, SUBFLOW_DEPTH, "exact subflow depth");
    assert_eq!(
        count_prompt_assets(recipe),
        DETACHED_ASSETS,
        "exact detached asset references"
    );
    let expected_access = match isolation {
        IsolationClass::SandboxIsolated => WorkspaceAccessMode::ReadOnly,
        IsolationClass::AuthorityBearing => WorkspaceAccessMode::Full,
    };
    assert_eq!(
        recipe.workspace.as_ref().expect("workspace").access,
        expected_access,
        "exact sandbox isolation class"
    );
    let asset_bytes = (0..DETACHED_ASSETS)
        .map(|index| {
            fs::metadata(
                Path::new(recipe.source_dir.as_ref().expect("recipe source directory"))
                    .join(format!("assets/prompt-{index:02}.txt")),
            )
            .expect("detached asset metadata")
            .len() as usize
        })
        .sum::<usize>();
    assert_eq!(
        fs::read_dir(
            Path::new(recipe.source_dir.as_ref().expect("recipe source directory"),).join("assets"),
        )
        .expect("read detached asset directory")
        .count(),
        DETACHED_ASSETS,
        "exact detached asset count"
    );
    assert_eq!(
        asset_bytes, DETACHED_ASSET_TOTAL_BYTES,
        "exact detached asset bytes"
    );
}

fn collect_topology(
    recipe: &Recipe,
    depth: usize,
    recipes: &mut usize,
    invokes: &mut Vec<(AdapterKind, String, String)>,
    invokes_by_depth: &mut BTreeMap<usize, usize>,
    max_depth: &mut usize,
) {
    *recipes += 1;
    *max_depth = (*max_depth).max(depth);
    for step in &recipe.steps {
        match &step.execution {
            StepExecutionDescriptor::Invoke {
                adapter,
                plugin,
                action,
                ..
            } => {
                invokes.push((*adapter, plugin.clone(), action.clone()));
                *invokes_by_depth.entry(depth).or_default() += 1;
            }
            StepExecutionDescriptor::Subflow { recipe } => {
                collect_topology(
                    recipe,
                    depth + 1,
                    recipes,
                    invokes,
                    invokes_by_depth,
                    max_depth,
                );
            }
            StepExecutionDescriptor::Control { .. } => {}
        }
    }
}

fn count_prompt_assets(recipe: &Recipe) -> usize {
    recipe
        .steps
        .iter()
        .map(|step| {
            usize::from(step.prompt_binding.is_some())
                + match &step.execution {
                    StepExecutionDescriptor::Subflow { recipe } => count_prompt_assets(recipe),
                    _ => 0,
                }
        })
        .sum()
}

fn assert_probe_counts(records: &[PreflightMeasurement], include_kernel: bool) {
    let counts = probe_counts(records);
    assert_eq!(
        counts.get(&PreflightMeasurementScope::ApplicationReadiness),
        Some(&(APPLICATION_PROBE_GROUPS, APPLICATION_PROBE_GROUPS)),
        "application readiness probe groups and probes"
    );
    let application_adapters =
        probe_counts_by_adapter(records, PreflightMeasurementScope::ApplicationReadiness);
    assert_eq!(
        application_adapters.get(&AdapterKind::Process),
        Some(&(
            APPLICATION_PROCESS_PROBE_GROUPS,
            APPLICATION_PROCESS_PROBE_GROUPS,
        )),
        "exact application process probes"
    );
    assert_eq!(
        application_adapters.get(&AdapterKind::Mcp),
        Some(&(APPLICATION_MCP_PROBE_GROUPS, APPLICATION_MCP_PROBE_GROUPS,)),
        "exact application MCP probes"
    );
    if include_kernel {
        assert_eq!(
            counts.get(&PreflightMeasurementScope::KernelRecipe { depth: 0 }),
            Some(&(ROOT_INVOKES, ROOT_INVOKES)),
            "root kernel recipe probes"
        );
        assert_eq!(
            counts.get(&PreflightMeasurementScope::KernelRecipe { depth: 1 }),
            Some(&(CHILD_INVOKES, CHILD_INVOKES)),
            "child kernel recipe probes"
        );
        assert_eq!(
            counts.get(&PreflightMeasurementScope::KernelRecipe { depth: 2 }),
            Some(&(GRANDCHILD_INVOKES, GRANDCHILD_INVOKES)),
            "grandchild kernel recipe probes"
        );
    } else {
        assert_eq!(counts.len(), 1, "readiness must not run kernel preflight");
    }
}

fn probe_counts_by_adapter(
    records: &[PreflightMeasurement],
    expected_scope: PreflightMeasurementScope,
) -> BTreeMap<AdapterKind, (usize, usize)> {
    let mut counts = BTreeMap::new();
    for record in records {
        if let PreflightMeasurement::ProbeBatch {
            scope,
            adapter_kind,
            probe_group_count,
            remote_probe_count,
        } = record
            && *scope == expected_scope
        {
            let entry = counts.entry(*adapter_kind).or_insert((0, 0));
            entry.0 += probe_group_count;
            entry.1 += remote_probe_count;
        }
    }
    counts
}

fn probe_counts(
    records: &[PreflightMeasurement],
) -> BTreeMap<PreflightMeasurementScope, (usize, usize)> {
    let mut counts = BTreeMap::new();
    for record in records {
        if let PreflightMeasurement::ProbeBatch {
            scope,
            probe_group_count,
            remote_probe_count,
            ..
        } = record
        {
            let entry = counts.entry(*scope).or_insert((0, 0));
            entry.0 += probe_group_count;
            entry.1 += remote_probe_count;
        }
    }
    counts
}

fn print_measurements(label: &str, records: &[PreflightMeasurement]) {
    let mut phases =
        BTreeMap::<(PreflightMeasurementScope, PreflightMeasurementPhase), u128>::new();
    for record in records {
        if let PreflightMeasurement::Phase {
            scope,
            phase,
            elapsed,
        } = record
        {
            *phases.entry((*scope, *phase)).or_default() += elapsed.as_nanos();
        }
    }
    eprintln!("preflight benchmark measurements: {label}");
    for (scope, (groups, probes)) in probe_counts(records) {
        eprintln!("  scope={scope:?} probe_groups={groups} process_or_remote_probes={probes}");
    }
    for ((scope, phase), elapsed_ns) in phases {
        eprintln!("  scope={scope:?} phase={phase:?} elapsed_ns={elapsed_ns}");
    }
}

fn measure_readiness(fixture: &BenchmarkFixture) -> Vec<PreflightMeasurement> {
    let (result, records) = capture_preflight_measurements(|| {
        fixture
            .context
            .evaluate_workflow_readiness(WORKFLOW_REF, BTreeMap::new(), false, false)
    });
    let validation = result.expect("evaluate generated workflow readiness");
    assert!(
        validation.is_valid(),
        "unexpected readiness blockers: {:?}",
        validation
            .checks
            .iter()
            .filter(|check| check.status == transist::ValidationStatus::Failed)
            .collect::<Vec<_>>()
    );
    fixture.assert_read_only_operation();
    assert_probe_counts(&records, false);
    records
}

fn measure_run_birth(fixture: &BenchmarkFixture) -> Vec<PreflightMeasurement> {
    let (result, records) = capture_preflight_measurements(|| {
        fixture
            .context
            .prepare_workflow(WORKFLOW_REF, Some(RUN_ID), BTreeMap::new())
    });
    let report = result.expect("prepare generated benchmark workflow");
    assert_eq!(report.run_id, RUN_ID);
    assert_probe_counts(&records, true);
    records
}

fn benchmark_workflow_readiness(c: &mut Criterion) {
    static PRINTED: OnceLock<()> = OnceLock::new();
    let baseline = BenchmarkFixture::new(IsolationClass::SandboxIsolated);
    let records = measure_readiness(&baseline);
    PRINTED.get_or_init(|| print_measurements("application_readiness", &records));

    let mut group = c.benchmark_group("workflow_readiness");
    group.sample_size(10);
    for isolation in [
        IsolationClass::SandboxIsolated,
        IsolationClass::AuthorityBearing,
    ] {
        group.bench_function(isolation.label(), |b| {
            b.iter_batched(
                || BenchmarkFixture::new(isolation),
                |fixture| {
                    let records = measure_readiness(&fixture);
                    black_box(records);
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn benchmark_run_birth(c: &mut Criterion) {
    static PRINTED: OnceLock<()> = OnceLock::new();
    let mut group = c.benchmark_group("workflow_run_birth");
    group.sample_size(10);
    group.bench_function("sandbox_isolated", |b| {
        b.iter_batched(
            || BenchmarkFixture::new(IsolationClass::SandboxIsolated),
            |fixture| {
                let records = measure_run_birth(&fixture);
                PRINTED.get_or_init(|| print_measurements("run_birth", &records));
                fixture.finish_run_birth();
                black_box(records);
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path).expect("plugin metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make plugin executable");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn benchmark_workflow_report_collection(c: &mut Criterion) {
    // Both collectors use the same generated installed tree; fixture creation is untimed.
    let fixture = BenchmarkFixture::new(IsolationClass::SandboxIsolated);
    let full = fixture
        .context
        .validate_bound_workflow(WORKFLOW_REF, BTreeMap::new(), false, false)
        .expect("full static validation");
    let summary = fixture
        .context
        .validate_bound_workflow_summary(WORKFLOW_REF, BTreeMap::new(), false, false)
        .expect("summary static validation");
    assert_eq!(summary.status, full.status);
    assert_eq!(
        summary.policy_suggestion.as_ref(),
        Some(&full.policy_suggestion)
    );
    fixture.assert_read_only_operation();
    let mut group = c.benchmark_group("workflow_report_collection");
    group.bench_function("summary", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .context
                    .validate_bound_workflow_summary(WORKFLOW_REF, BTreeMap::new(), false, false)
                    .expect("summary static validation"),
            )
        })
    });
    group.bench_function("full", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .context
                    .validate_bound_workflow(WORKFLOW_REF, BTreeMap::new(), false, false)
                    .expect("full static validation"),
            )
        })
    });
    group.finish();
    fixture.assert_read_only_operation();
}

fn assert_fast_work(counts: &BTreeMap<transist_plugins::ValidationWork, usize>) {
    use transist_plugins::ValidationWork as Work;
    for work in [Work::DefinitionValidation, Work::ContractValidation] {
        assert_eq!(
            counts.get(&work).copied().unwrap_or_default(),
            0,
            "{counts:?}"
        );
    }
    for work in [
        Work::PackageLoad,
        Work::PackageHash,
        Work::SchemaCompilation,
    ] {
        assert_eq!(counts.get(&work), Some(&PLUGIN_PACKAGES), "{counts:?}");
    }
    assert_eq!(counts.get(&Work::InputValidation), Some(&REACHABLE_INVOKES));
    assert_eq!(counts.get(&Work::InputBinding), Some(&1));
    assert_eq!(counts.get(&Work::PolicyEvaluation), Some(&1));
}

fn fresh_cli_batch(fixture: &BenchmarkFixture, cli: &Path, command: &str, live: bool) {
    for _ in 0..10 {
        let mut child = Command::new(cli);
        child
            .current_dir(fixture.root.path())
            .env("TMPDIR", "/tmp")
            .args([
                "--home",
                fixture.home.to_str().expect("home path"),
                "workflows",
                command,
                WORKFLOW_REF,
                "--format",
                "json",
            ])
            .stderr(Stdio::piped());
        if command == "authorize" {
            child.arg("--dry-run");
        }
        if live {
            child.arg("--live");
        }
        let output = child.output().expect("start fresh CLI process");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("CLI JSON report");
        if command == "authorize" {
            assert_eq!(report["status"], "already_authorized");
        } else if live {
            assert_eq!(report["status"], "ready");
        } else {
            assert_eq!(report["validation_status"], "passed");
            assert_eq!(report["evaluation_mode"], "validation");
        }
    }
}

fn benchmark_workflow_validation_fast_path(c: &mut Criterion) {
    let fixture = BenchmarkFixture::new(IsolationClass::SandboxIsolated);
    fixture
        .context
        .install_flow_file_with_trust(&fixture.root.path().join("source/workflow.yaml"), false)
        .expect("install receipt-backed fixture");
    let index: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.home.join("registry/workflow-install-index.json"))
            .expect("install index"),
    )
    .expect("index JSON");
    assert_eq!(
        index["entries"][WORKFLOW_REF]["definition_validation"]["validator_revision"],
        2
    );
    assert_eq!(
        index["entries"][WORKFLOW_REF]["definition_validation"]["plugin_package_digests"]
            .as_object()
            .expect("package receipts")
            .len(),
        PLUGIN_PACKAGES
    );
    // Inspect structural work outside timing; timing itself has no observer overhead.
    let ((fast, probes), counts) = transist_plugins::capture_validation_work(|| {
        capture_preflight_measurements(|| {
            fixture
                .context
                .workflow_validation_report(WORKFLOW_REF, BTreeMap::new(), false, false)
                .expect("fast report")
        })
    });
    assert!(fast.is_validation_passed());
    assert_fast_work(&counts);
    assert!(probe_counts(&probes).is_empty());
    eprintln!("workflow_validation_fast_path validation work={counts:?} probes=0");
    let ((authorization, probes), counts) = transist_plugins::capture_validation_work(|| {
        capture_preflight_measurements(|| {
            fixture
                .context
                .authorize_workflow_summary(WORKFLOW_REF, BTreeMap::new(), None, false)
                .expect("authorization")
        })
    });
    assert_eq!(
        authorization.status,
        transist::WorkflowAuthorizationStatus::AlreadyAuthorized
    );
    assert_fast_work(&counts);
    assert!(probe_counts(&probes).is_empty());
    eprintln!("workflow_validation_fast_path already_authorized work={counts:?} probes=0");
    let ((live, probes), counts) = transist_plugins::capture_validation_work(|| {
        capture_preflight_measurements(|| {
            fixture
                .context
                .workflow_readiness_report(WORKFLOW_REF, BTreeMap::new(), false, false)
                .expect("live report")
        })
    });
    assert!(live.is_ready());
    assert_eq!(
        fast.validation
            .as_ref()
            .expect("fast validation")
            .missing_capabilities,
        live.validation
            .as_ref()
            .expect("live validation")
            .missing_capabilities
    );
    assert_probe_counts(&probes, false);
    eprintln!(
        "workflow_validation_fast_path live work={counts:?} probes={:?}",
        probe_counts_by_adapter(&probes, PreflightMeasurementScope::ApplicationReadiness)
    );
    let mut group = c.benchmark_group("workflow_validation_fast_path");
    group.sample_size(10);
    group.bench_function("validation", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .context
                    .workflow_validation_summary(WORKFLOW_REF, BTreeMap::new(), false, false)
                    .expect("validation"),
            )
        })
    });
    group.bench_function("already_authorized", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .context
                    .authorize_workflow_summary(WORKFLOW_REF, BTreeMap::new(), None, false)
                    .expect("authorization"),
            )
        })
    });
    group.bench_function("live_readiness", |b| {
        b.iter(|| {
            black_box(
                fixture
                    .context
                    .workflow_readiness_summary(WORKFLOW_REF, BTreeMap::new(), false, false)
                    .expect("live readiness"),
            )
        })
    });
    // Cargo's all-target test smoke omits Criterion's --bench flag before CLI build.
    // Fresh-process timing belongs to cargo bench; structural API assertions above
    // still execute in test mode, without depending on a pre-existing CLI binary.
    if std::env::args().any(|arg| arg == "--bench") {
        let cli = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/transist");
        assert!(
            cli.is_file(),
            "build the pinned CLI/admin before running fresh-process benchmarks"
        );
        for (name, command, live) in [
            ("fresh_process_10_checks", "check", false),
            ("fresh_process_10_authorizations", "authorize", false),
            ("fresh_process_10_live_checks", "check", true),
        ] {
            group.bench_function(name, |b| {
                b.iter(|| fresh_cli_batch(&fixture, &cli, command, live))
            });
        }
    }
    group.finish();
    fixture.assert_read_only_operation();
}

criterion_group!(
    benches,
    benchmark_workflow_readiness,
    benchmark_run_birth,
    benchmark_workflow_report_collection,
    benchmark_workflow_validation_fast_path
);
criterion_main!(benches);
