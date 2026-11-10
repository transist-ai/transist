// SPDX-License-Identifier: MPL-2.0
use transist_compiler::{
    Flow, compile_flow,
    graph::{WorkflowEdgeKind, flow_graph, recipe_graph},
};

fn flow(condition: &str, terminal: bool) -> Flow {
    let successor = if terminal {
        ""
    } else {
        "- id: successor\n  control: control.noop\n- id: finish\n  control: control.finish\n"
    };
    yaml_serde::from_str(&format!(
        r#"
schema: transist.flow.v1
id: projection
version: v1
steps:
- id: loop
  control: control.noop
  loop:
    {condition}
    max_iters: 2
    body: [{{ref: ' body '}}, {{ref: body2}}]
- id: body
  control: control.noop
- id: body2
  control: control.noop
{successor}"#
    ))
    .unwrap()
}

#[test]
fn loop_cap_routing_authored_and_marked_recipe_graphs_have_exact_parity() {
    for condition in [
        "until: 'false'",
        "while: 'true'",
        "until: 'true'",
        "while: 'false'",
    ] {
        for terminal in [false, true] {
            let flow = flow(condition, terminal);
            let recipe = compile_flow(&flow).unwrap();
            let authored = flow_graph::edges_from(&flow);
            let compiled = recipe_graph::edges_from(&recipe);
            assert_eq!(authored, compiled, "{condition}, terminal={terminal}");
            assert_eq!(
                compiled
                    .iter()
                    .filter(|edge| edge.loop_routing_version == Some(1))
                    .count(),
                if terminal { 1 } else { 2 }
            );
            let repeat = compiled
                .iter()
                .find(|edge| edge.kind == WorkflowEdgeKind::Repeat)
                .unwrap();
            assert_eq!(repeat.loop_max_iters, Some(2));
            assert_eq!(repeat.loop_routing_version, None);
        }
    }
}

#[test]
fn loop_cap_routing_marked_classification_uses_roles_while_legacy_keeps_guard_inference() {
    let mut recipe = compile_flow(&flow("until: 'false'", false)).unwrap();
    let graph = recipe.graph.as_mut().unwrap();
    graph.edges[0].guard = Some("arbitrary == true".into());
    graph.edges[1].guard = Some("NOT (arbitrary == true)".into());
    let marked = recipe_graph::edges_from(&recipe);
    assert_eq!(marked[0].kind, WorkflowEdgeKind::Loop);
    assert_eq!(marked[1].kind, WorkflowEdgeKind::Exit);
    assert_eq!(marked[0].guard.as_deref(), Some("arbitrary == true"));
    let graph = recipe.graph.as_mut().unwrap();
    graph.loop_routing_version = None;
    for edge in &mut graph.edges {
        edge.loop_route = None;
    }
    let legacy = recipe_graph::edges_from(&recipe);
    assert_eq!(legacy[0].kind, WorkflowEdgeKind::Then);
    assert_eq!(legacy[1].kind, WorkflowEdgeKind::Else);
    assert!(
        legacy
            .iter()
            .all(|edge| edge.loop_routing_version.is_none())
    );
    let repeat = legacy
        .iter()
        .find(|edge| edge.kind == WorkflowEdgeKind::Repeat)
        .unwrap();
    assert_eq!(
        repeat.loop_max_iters, None,
        "legacy repeat metadata must not be enriched"
    );
}

#[test]
fn loop_cap_routing_graph_parity_keeps_surrounding_conditionals_ordinary() {
    let source = r#"
schema: transist.flow.v1
id: mixed
version: v1
steps:
- id: choose
  control: control.noop
  if:
    guard: 'true'
    then: [{ref: ' selected '}]
- id: selected
  control: control.noop
- id: first
  control: control.noop
  loop:
    until: 'false'
    max_iters: 1
    body: [{ref: body}]
- id: body
  control: control.noop
- id: second
  control: control.noop
  loop:
    while: 'true'
    max_iters: 2
    body: [{ref: tail}]
- id: tail
  control: control.noop
"#;
    let flow: Flow = yaml_serde::from_str(source).unwrap();
    let recipe = compile_flow(&flow).unwrap();
    let edges = flow_graph::edges_from(&flow);
    assert_eq!(edges, recipe_graph::edges_from(&recipe));
    assert!(
        edges
            .iter()
            .filter(|edge| edge.from_step_id == "choose" || edge.from_step_id == "selected")
            .all(|edge| edge.loop_routing_version.is_none())
    );
}
