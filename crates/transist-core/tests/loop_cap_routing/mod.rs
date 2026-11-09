// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn loop_cap_routing_frozen_unmarked_recipe_preserves_historical_skipped_successor() {
    let recipe: Recipe = serde_json::from_str(include_str!("legacy.recipe.json")).unwrap();
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let home = test_home_dir(&tmp);
    let summary = kernel
        .execute_recipe(tmp.path(), &home, &recipe, Some("legacy"))
        .unwrap();
    assert_eq!(summary.status, KernelRunStatus::Success);
    let state = run_store.run_json("legacy");
    for (id, count) in [
        ("loop", 2),
        ("body", 1),
        ("mandatory_successor", 0),
        ("finish", 0),
    ] {
        assert_eq!(state["steps"][id]["executions"], count, "{id}");
    }
    assert_eq!(state["steps"]["mandatory_successor"]["status"], "skipped");
    assert_eq!(state["steps"]["finish"]["status"], "skipped");
    assert!(execution.invocations().is_empty());
}

#[test]
fn loop_cap_routing_kernel_observes_variable_mutations_before_cap() {
    use transist_protocol::{RecipeLoopRoute, RecipeLoopRouteRole};
    let mut recipe: Recipe = serde_json::from_str(include_str!("legacy.recipe.json")).unwrap();
    let predicate = "vars.done == true";
    recipe.steps[0].with.insert(
        "__transist_loop".into(),
        serde_json::json!({
            "body_steps":["body"], "max_iters":5, "until":predicate
        })
        .into(),
    );
    let mut seed = invoke_step("seed");
    seed.vars.insert("done".into(), KernelValue::Bool(false));
    recipe.steps[1] = invoke_step("body");
    recipe.steps[1]
        .vars
        .insert("done".into(), KernelValue::Bool(true));
    recipe.steps.insert(0, seed);
    let graph = recipe.graph.as_mut().unwrap();
    graph.entry = "seed".into();
    graph.loop_routing_version = Some(1);
    graph.edges[0].guard = Some(format!("NOT ({predicate})"));
    graph.edges[1].guard = Some(predicate.into());
    for (edge, role) in graph
        .edges
        .iter_mut()
        .take(2)
        .zip([RecipeLoopRouteRole::Enter, RecipeLoopRouteRole::Exit])
    {
        edge.loop_route = Some(RecipeLoopRoute {
            owner_step_id: "loop".into(),
            role,
        });
    }
    graph.edges.push(RecipeGraphEdge {
        from: "seed".into(),
        to: "loop".into(),
        event: "on_success".into(),
        guard: None,
        r#loop: None,
        loop_route: None,
    });
    let execution = Arc::new(RecordingExecution::default());
    let run_store = Arc::new(MemoryRunStore::default());
    let kernel = test_kernel(
        execution.clone(),
        run_store.clone(),
        Arc::new(RecordingEvents::default()),
    );
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let home = test_home_dir(&tmp);
    let summary = kernel
        .execute_recipe(tmp.path(), &home, &recipe, Some("mutated"))
        .unwrap();
    assert_eq!(summary.status, KernelRunStatus::Success);
    let state = run_store.run_json("mutated");
    for (id, count) in [
        ("seed", 1),
        ("loop", 2),
        ("body", 1),
        ("mandatory_successor", 1),
        ("finish", 1),
    ] {
        assert_eq!(state["steps"][id]["executions"], count, "{id}: {state}");
    }
    assert_eq!(execution.invocations().len(), 2);
}
