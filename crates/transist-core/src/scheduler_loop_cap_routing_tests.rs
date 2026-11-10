// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::execution_graph::build_execution_graph;
use transist_protocol::Recipe;

fn loop_recipe(until: bool, predicate: &str, cap: u32) -> Recipe {
    let (enter, exit) = if until {
        (format!("NOT ({predicate})"), predicate.to_string())
    } else {
        (predicate.to_string(), format!("NOT ({predicate})"))
    };
    let mut recipe: Recipe = serde_json::from_value(serde_json::json!({
        "schema":"transist.recipe.v1", "workflow":{"id":"loop_test", "version":"v1"},
        "steps":[
            {"id":"loop", "execution":{"kind":"control", "primitive":"control.noop"},
             "with":{"__transist_loop":{"max_iters":cap, "body_steps":["body"]}}},
            {"id":"body", "execution":{"kind":"control", "primitive":"control.noop"}},
            {"id":"next", "execution":{"kind":"control", "primitive":"control.noop"}},
            {"id":"finish", "execution":{"kind":"control", "primitive":"control.finish"}}
        ],
        "graph":{"entry":"loop", "loop_routing_version":1, "edges":[
            {"from":"loop", "to":"body", "event":"on_success", "guard":enter,
             "loop_route":{"owner_step_id":"loop", "role":"enter"}},
            {"from":"loop", "to":"next", "event":"on_success", "guard":exit,
             "loop_route":{"owner_step_id":"loop", "role":"exit"}},
            {"from":"body", "to":"loop", "event":"on_success", "loop":{}},
            {"from":"next", "to":"finish", "event":"on_success", "guard":"false"}
        ]}
    }))
    .unwrap();
    let KernelValue::Object(meta) = recipe.steps[0].with.get_mut("__transist_loop").unwrap() else {
        unreachable!()
    };
    meta.insert(
        if until { "until" } else { "while" }.into(),
        KernelValue::String(predicate.to_string()),
    );
    recipe
}

fn snapshot(recipe: &Recipe, executions: u32, baseline: u32) -> KernelRunSnapshot {
    let mut snapshot = KernelRunSnapshot::new(
        "run",
        "loop_test",
        "v1",
        &recipe
            .steps
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
    );
    let owner = snapshot.steps.get_mut("loop").unwrap();
    owner.status = KernelStepStatus::Succeeded;
    owner.executions = executions;
    owner.activation_executions_baseline = baseline;
    snapshot
}

fn assert_routes(
    graph: &ExecutionGraph,
    snapshot: &KernelRunSnapshot,
    vars: &BTreeMap<String, KernelValue>,
    enter: bool,
    exit: bool,
) {
    for (target, expected) in [("body", enter), ("next", exit)] {
        let incoming = incoming_edge_reachability(graph, snapshot, &BTreeMap::new(), vars, target);
        assert_eq!(
            incoming.has_satisfied_edge, expected,
            "{target}: {incoming:?}, guards: {:?}",
            graph.nodes["loop"].outgoing
        );
    }
}

#[test]
fn loop_cap_routing_caps_block_entry_and_satisfy_exit_using_activation_counts() {
    for (until, predicate) in [(true, "false"), (false, "true")] {
        for cap in [1, 2] {
            let recipe = loop_recipe(until, predicate, cap);
            let graph = build_execution_graph(&recipe).unwrap();
            for baseline in [0, 40] {
                // A high lifetime count must not exhaust a fresh activation.
                assert_routes(
                    &graph,
                    &snapshot(&recipe, baseline + cap - 1, baseline),
                    &BTreeMap::new(),
                    true,
                    false,
                );
                for executions in [baseline + cap, baseline + cap + 1] {
                    let snapshot = snapshot(&recipe, executions, baseline);
                    assert_routes(&graph, &snapshot, &BTreeMap::new(), false, true);
                    assert_eq!(
                        runnable_frontier(&graph, &snapshot, &BTreeMap::new(), &BTreeMap::new(), 0),
                        vec!["next"]
                    );
                }
            }
        }
    }
}

#[test]
fn loop_cap_routing_preserves_early_predicate_exits() {
    for (until, predicate) in [(true, "true"), (false, "false")] {
        let recipe = loop_recipe(until, predicate, 3);
        let graph = build_execution_graph(&recipe).unwrap();
        assert_routes(
            &graph,
            &snapshot(&recipe, 1, 0),
            &BTreeMap::new(),
            false,
            true,
        );
    }
}

#[test]
fn loop_cap_routing_evaluates_mutable_variables_freshly_below_cap() {
    for until in [false, true] {
        let recipe = loop_recipe(until, "vars.ready == true", 5);
        let graph = build_execution_graph(&recipe).unwrap();
        let snapshot = snapshot(&recipe, 2, 0);
        for ready in [false, true, false] {
            let vars = BTreeMap::from([("ready".into(), serde_json::json!(ready).into())]);
            let exit = if until { ready } else { !ready };
            assert_routes(&graph, &snapshot, &vars, !exit, exit);
        }
    }
}

#[test]
fn loop_cap_routing_evaluates_current_step_outputs_freshly_below_cap() {
    let recipe = loop_recipe(true, "steps.body.out.ready == true", 5);
    let graph = build_execution_graph(&recipe).unwrap();
    let mut snapshot = snapshot(&recipe, 2, 0);
    for ready in [false, true, false, true] {
        let body = snapshot.steps.get_mut("body").unwrap();
        body.status = KernelStepStatus::Succeeded;
        body.executions = 1;
        body.last_result = Some(
            serde_json::from_value(serde_json::json!({
                "type":transist_protocol::TEP_INVOKE_RESULT_TYPE_V1,
                "protocol_version":transist_protocol::TEP_PROTOCOL_VERSION_V1, "invoke_id":"invoke",
                "run_id":"run", "step_id":"body", "execution":1, "status":"success",
                "exit_code":0, "stdout":"", "stderr":"", "artifacts":[], "evidence":[],
                "timing":{"started_at":"0", "finished_at":"0", "duration_ms":0},
                "output":{"ready":ready}
            }))
            .unwrap(),
        );
        assert_routes(&graph, &snapshot, &BTreeMap::new(), !ready, ready);
    }
    // Resetting the declared body makes its current output known null. It must
    // never reuse the earlier true result to authorize a successful exit.
    let body = snapshot.steps.get_mut("body").unwrap();
    body.status = KernelStepStatus::Pending;
    body.last_result = None;
    assert_routes(&graph, &snapshot, &BTreeMap::new(), true, false);
}

#[test]
fn loop_cap_routing_empty_body_projection_is_distinct_from_unknown_output() {
    let recipe = loop_recipe(true, "steps.body.out.decision == 'approved'", 3);
    let graph = build_execution_graph(&recipe).unwrap();
    let mut snapshot = snapshot(&recipe, 1, 0);
    assert_routes(&graph, &snapshot, &BTreeMap::new(), true, false);
    for status in [
        KernelStepStatus::Blocked,
        KernelStepStatus::RetryWait,
        KernelStepStatus::Runnable,
    ] {
        snapshot.steps.get_mut("body").unwrap().status = status;
        assert_routes(&graph, &snapshot, &BTreeMap::new(), true, false);
    }
    snapshot.steps.get_mut("body").unwrap().status = KernelStepStatus::Running;
    assert_routes(&graph, &snapshot, &BTreeMap::new(), false, false);
    let body = snapshot.steps.get_mut("body").unwrap();
    body.status = KernelStepStatus::Succeeded;
    body.executions = 1;
    body.last_result = Some(serde_json::from_value(serde_json::json!({
        "type":transist_protocol::TEP_INVOKE_RESULT_TYPE_V1,
        "protocol_version":transist_protocol::TEP_PROTOCOL_VERSION_V1,
        "invoke_id":"invoke", "run_id":"run", "step_id":"body", "execution":1,
        "status":"success", "output":{}, "timing":{"started_at":"0", "finished_at":"0", "duration_ms":0}
    })).unwrap());
    // A missing field on an actual completed result is unknown, not an empty
    // reset projection, and must authorize neither branch below the cap.
    assert_routes(&graph, &snapshot, &BTreeMap::new(), false, false);
    snapshot
        .steps
        .get_mut("body")
        .unwrap()
        .last_result
        .as_mut()
        .unwrap()
        .output = serde_json::json!({"decision":"approved"});
    assert_routes(&graph, &snapshot, &BTreeMap::new(), false, true);
    let body = snapshot.steps.get_mut("body").unwrap();
    body.status = KernelStepStatus::Pending;
    body.last_result = None;
    assert_routes(&graph, &snapshot, &BTreeMap::new(), true, false);
}

#[test]
fn loop_cap_routing_unknown_and_malformed_predicates_authorize_neither_branch_below_cap() {
    for predicate in [
        "inputs.missing",
        "steps.undeclared.out.missing == true",
        "vars.missing == true",
        "(",
        "true &&",
        "unknown_predicate",
        "'unterminated",
    ] {
        for until in [false, true] {
            let recipe = loop_recipe(until, predicate, 2);
            let graph = build_execution_graph(&recipe).unwrap();
            assert_routes(
                &graph,
                &snapshot(&recipe, 1, 0),
                &BTreeMap::new(),
                false,
                false,
            );
            assert_routes(
                &graph,
                &snapshot(&recipe, 2, 0),
                &BTreeMap::new(),
                false,
                true,
            );
        }
    }
}

#[test]
fn loop_cap_routing_failed_canceled_and_unfinished_owners_never_authorize_cap_exit() {
    let recipe = loop_recipe(true, "false", 2);
    let graph = build_execution_graph(&recipe).unwrap();
    for status in [
        KernelStepStatus::Failed,
        KernelStepStatus::Canceled,
        KernelStepStatus::Skipped,
        KernelStepStatus::Pending,
        KernelStepStatus::Runnable,
        KernelStepStatus::Dispatching,
        KernelStepStatus::Running,
        KernelStepStatus::Blocked,
        KernelStepStatus::RetryWait,
    ] {
        let mut snapshot = snapshot(&recipe, 2, 0);
        snapshot.steps.get_mut("loop").unwrap().status = status;
        assert_routes(&graph, &snapshot, &BTreeMap::new(), false, false);
        if matches!(
            status,
            KernelStepStatus::Failed | KernelStepStatus::Canceled
        ) {
            assert!(
                incoming_edge_reachability(
                    &graph,
                    &snapshot,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    "next"
                )
                .has_dependency_failed_edge
            );
        }
    }
}

#[test]
fn loop_cap_routing_ordinary_edges_and_unmarked_loops_keep_guard_semantics() {
    let mut recipe = loop_recipe(true, "false", 2);
    let mut graph = build_execution_graph(&recipe).unwrap();
    let mut snapshot = snapshot(&recipe, 2, 0);
    let next = snapshot.steps.get_mut("next").unwrap();
    next.status = KernelStepStatus::Succeeded;
    next.executions = 100;
    let incoming = incoming_edge_reachability(
        &graph,
        &snapshot,
        &BTreeMap::new(),
        &BTreeMap::new(),
        "finish",
    );
    assert!(!incoming.has_satisfied_edge);
    assert!(incoming.has_guard_blocked_edge);
    graph.nodes.get_mut("next").unwrap().outgoing[0].guard = Some("inputs.missing".into());
    assert!(
        incoming_edge_reachability(
            &graph,
            &snapshot,
            &BTreeMap::new(),
            &BTreeMap::new(),
            "finish"
        )
        .has_satisfied_edge,
        "ordinary guards must retain the legacy literal fallback even inside marked graphs"
    );
    let graph = recipe.graph.as_mut().unwrap();
    graph.loop_routing_version = None;
    for edge in &mut graph.edges {
        edge.loop_route = None;
    }
    // Serialize/load a frozen unmarked recipe instead of asking the compiler
    // to reconstruct an older recipe with current defaults.
    let yaml = yaml_serde::to_string(&recipe).unwrap();
    let frozen: Recipe = yaml_serde::from_str(&yaml).unwrap();
    let graph = build_execution_graph(&frozen).unwrap();
    assert_routes(&graph, &snapshot, &BTreeMap::new(), true, false);
    assert_eq!(yaml_serde::to_string(&frozen).unwrap(), yaml);
}

#[test]
fn loop_cap_routing_preserves_short_circuits_and_unquoted_comparison_literals() {
    for (predicate, exit) in [
        ("false && inputs.missing", false),
        ("true || inputs.missing", true),
        ("vars.phase == done", true),
        ("vars.phase == waiting", false),
    ] {
        let recipe = loop_recipe(true, predicate, 5);
        let graph = build_execution_graph(&recipe).unwrap();
        let vars = BTreeMap::from([("phase".into(), KernelValue::String("done".into()))]);
        assert_routes(&graph, &snapshot(&recipe, 1, 0), &vars, !exit, exit);
    }
}
