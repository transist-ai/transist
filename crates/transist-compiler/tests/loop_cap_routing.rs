// SPDX-License-Identifier: MPL-2.0
use transist_compiler::{Flow, compile_flow};
use transist_protocol::{RecipeLoopRouteRole, validate_recipe_loop_routing};

fn compile(source: &str) -> transist_protocol::Recipe {
    let flow: Flow = yaml_serde::from_str(source).unwrap();
    let recipe = compile_flow(&flow).unwrap();
    validate_recipe_loop_routing(&recipe).unwrap();
    recipe
}

#[test]
fn loop_cap_routing_compiler_emits_complete_roles_and_preserves_guards_and_repeat() {
    for (condition, enter, exit) in [
        ("until: 'false'", "NOT (false)", "false"),
        ("while: 'true'", "true", "NOT (true)"),
    ] {
        for terminal in [false, true] {
            let successor = if terminal {
                ""
            } else {
                "\n- id: next\n  control: control.finish\n"
            };
            let recipe = compile(&format!(
                r#"
schema: transist.flow.v1
id: loop_compile
version: v1
steps:
- id: loop
  control: control.noop
  loop:
    {condition}
    max_iters: 2
    max_duration: 1s
    body: [{{ref: body}}, {{ref: body2}}]
- id: body
  control: control.noop
- id: body2
  control: control.noop
{successor}"#
            ));
            let graph = recipe.graph.as_ref().unwrap();
            assert_eq!(graph.loop_routing_version, Some(1));
            let edges: Vec<_> = graph
                .edges
                .iter()
                .filter(|edge| edge.from == "loop")
                .collect();
            assert_eq!(edges.len(), if terminal { 1 } else { 2 });
            assert_eq!(edges[0].to, "body");
            assert_eq!(edges[0].guard.as_deref(), Some(enter));
            let route = edges[0].loop_route.as_ref().unwrap();
            assert_eq!(route.owner_step_id, "loop");
            assert_eq!(route.role, RecipeLoopRouteRole::Enter);
            if !terminal {
                assert_eq!(edges[1].to, "next");
                assert_eq!(edges[1].guard.as_deref(), Some(exit));
                let route = edges[1].loop_route.as_ref().unwrap();
                assert_eq!(route.owner_step_id, "loop");
                assert_eq!(route.role, RecipeLoopRouteRole::Exit);
            }
            let repeat = graph
                .edges
                .iter()
                .find(|edge| edge.from == "body2")
                .unwrap();
            assert_eq!(repeat.to, "loop");
            assert_eq!(repeat.guard, None);
            assert_eq!(repeat.loop_route, None);
            assert_eq!(
                repeat.r#loop.as_ref().unwrap(),
                &transist_protocol::RecipeLoop {
                    while_expr: None,
                    until_expr: None,
                    max_iters: None,
                }
            );
            assert_eq!(
                recipe.steps[0].with["__transist_loop"].clone().to_json()["max_duration_ms"],
                1000
            );
        }
    }
}

#[test]
fn loop_cap_routing_compiler_marks_all_wrappers_and_normalizes_validated_refs() {
    // Authoring validation accepts padded refs; recipe endpoints and wrapper
    // body metadata must use the same normalized identities.
    let recipe = compile(
        r#"
schema: transist.flow.v1
id: multiple_loops
version: v1
steps:
- id: branch
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
    body: [{ref: ' body '}]
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
"#,
    );
    let graph = recipe.graph.as_ref().unwrap();
    let roles: Vec<_> = graph
        .edges
        .iter()
        .filter_map(|edge| {
            edge.loop_route
                .as_ref()
                .map(|route| (route.owner_step_id.as_str(), route.role))
        })
        .collect();
    assert_eq!(
        roles,
        vec![
            ("first", RecipeLoopRouteRole::Enter),
            ("first", RecipeLoopRouteRole::Exit),
            ("second", RecipeLoopRouteRole::Enter)
        ]
    );
    assert!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.from == "branch" || edge.from == "selected")
            .all(|edge| edge.loop_route.is_none())
    );
}

#[test]
fn loop_cap_routing_compiler_leaves_loop_free_graph_unmarked() {
    let recipe = compile(
        r#"
schema: transist.flow.v1
id: ordinary
version: v1
steps:
- id: branch
  control: control.noop
  if:
    guard: 'true'
    then: [{ref: selected}]
- id: selected
  control: control.noop
- id: finish
  control: control.finish
"#,
    );
    let graph = recipe.graph.as_ref().unwrap();
    assert_eq!(graph.loop_routing_version, None);
    assert!(graph.edges.iter().all(|edge| edge.loop_route.is_none()));
    let yaml = yaml_serde::to_string(&recipe).unwrap();
    assert!(!yaml.contains("loop_routing_version"));
    assert!(!yaml.contains("loop_route"));
}

#[test]
fn loop_cap_routing_compiler_validates_nested_recipes_independently() {
    use transist_protocol::StepExecutionDescriptor;
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::write(
        tmp.path().join("child.yaml"),
        r#"
schema: transist.module.v1
id: child
version: v1
steps:
- id: child_loop
  control: control.noop
  loop:
    until: 'false'
    max_iters: 2
    body: [{ref: child_body}]
- id: child_body
  control: control.noop
- id: child_finish
  control: control.finish
"#,
    )
    .unwrap();
    for parent_loop in [false, true] {
        let loop_block = if parent_loop {
            "- id: parent_loop\n  control: control.noop\n  loop:\n    while: 'true'\n    max_iters: 2\n    body: [{ref: call_child}]\n"
        } else {
            ""
        };
        let source = format!(
            r#"
schema: transist.flow.v1
id: parent
version: v1
modules:
  child:
    path: child.yaml
steps:
{loop_block}- id: call_child
  module: child
- id: finish
  control: control.finish
"#
        );
        let path = tmp.path().join("parent.yaml");
        std::fs::write(&path, source).unwrap();
        let flow = transist_compiler::load_flow_unvalidated(&path).unwrap();
        let recipe = compile_flow(&flow).unwrap();
        validate_recipe_loop_routing(&recipe).unwrap();
        assert_eq!(
            recipe.graph.as_ref().unwrap().loop_routing_version,
            parent_loop.then_some(1)
        );
        let StepExecutionDescriptor::Subflow { recipe: child } =
            &recipe.steps[usize::from(parent_loop)].execution
        else {
            panic!("subflow")
        };
        assert_eq!(child.graph.as_ref().unwrap().loop_routing_version, Some(1));
        assert_eq!(
            child
                .graph
                .as_ref()
                .unwrap()
                .edges
                .iter()
                .filter(|edge| edge.loop_route.is_some())
                .count(),
            2
        );
    }
}
