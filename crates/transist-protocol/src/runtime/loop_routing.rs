// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::collections::BTreeSet;

// `default` handles omission; a present value must deserialize as T, never null.
pub(super) fn present_metadata<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Validates opt-in loop routing in this recipe and every embedded child/cleanup.
/// Returns an error before graph consumption for unsupported or inconsistent
/// metadata. Legacy graphs are neither repaired nor topology-revalidated.
pub fn validate_recipe_loop_routing(recipe: &Recipe) -> Result<(), String> {
    if let Some(graph) = &recipe.graph {
        validate_graph(recipe, graph).map_err(|error| {
            format!(
                "recipe '{}': invalid loop routing: {error}",
                recipe.workflow.id
            )
        })?;
    }
    for step in recipe
        .steps
        .iter()
        .chain(recipe.cleanup.iter().map(|c| &c.step))
    {
        if let StepExecutionDescriptor::Subflow { recipe } = &step.execution {
            validate_recipe_loop_routing(recipe)?;
        }
    }
    Ok(())
}

fn validate_graph(recipe: &Recipe, graph: &RecipeGraph) -> Result<(), String> {
    match graph.loop_routing_version {
        None => {
            return if graph.edges.iter().any(|edge| edge.loop_route.is_some()) {
                Err("edge metadata requires loop_routing_version".into())
            } else {
                Ok(())
            };
        }
        Some(1) => {}
        Some(version) => return Err(format!("unsupported loop_routing_version {version}")),
    }
    let ids: BTreeSet<_> = recipe.steps.iter().map(|step| step.id.as_str()).collect();
    if ids.len() != recipe.steps.len() || !ids.contains(graph.entry.as_str()) {
        return Err("missing entry or duplicate step ids".into());
    }
    for edge in &graph.edges {
        if !ids.contains(edge.from.as_str()) || !ids.contains(edge.to.as_str()) {
            return Err("edge source or target does not exist".into());
        }
        if let Some(route) = &edge.loop_route {
            if route.owner_step_id != edge.from || !ids.contains(route.owner_step_id.as_str()) {
                return Err("route owner must exist and be the edge source".into());
            }
        }
    }
    let mut owners = BTreeSet::new();
    let mut body_members = BTreeSet::new();
    for (index, step) in recipe.steps.iter().enumerate() {
        let Some(meta) = step.with.get("__transist_loop") else {
            continue;
        };
        let KernelValue::Object(meta) = meta else {
            return Err(format!("loop '{}' has malformed wrapper metadata", step.id));
        };
        // Both control and subflow wrappers are supported; invokes are not loops.
        if !matches!(&step.execution, StepExecutionDescriptor::Control { primitive } if primitive == "control.noop")
            && !matches!(step.execution, StepExecutionDescriptor::Subflow { .. })
        {
            return Err("loop owner must be a control.noop or subflow wrapper".into());
        }
        if !matches!(meta.get("max_iters"), Some(KernelValue::Number(n)) if n.as_u64().is_some_and(|n| n > 0 && n <= u32::MAX as u64))
        {
            return Err("loop owner requires a positive max_iters".into());
        }
        let Some(KernelValue::Array(body)) = meta.get("body_steps") else {
            return Err("loop owner requires body_steps".into());
        };
        let body: Vec<&str> = body
            .iter()
            .map(|value| match value {
                KernelValue::String(id) => Ok(id.as_str()),
                _ => Err("body step id must be a string".to_string()),
            })
            .collect::<Result<_, _>>()?;
        if body.is_empty()
            || body.iter().enumerate().any(|(offset, id)| {
                !ids.contains(id)
                    || !body_members.insert(*id)
                    || recipe.steps.get(index + offset + 1).map(|s| s.id.as_str()) != Some(*id)
            })
        {
            return Err(
                "loop body must be nonempty, unique and match ordered recipe topology".into(),
            );
        }
        owners.insert(step.id.as_str());
        let successor = recipe
            .steps
            .get(index + body.len() + 1)
            .map(|s| s.id.as_str());
        let outgoing: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.from == step.id)
            .collect();
        if outgoing.len() != 1 + usize::from(successor.is_some()) {
            return Err("loop wrapper has incomplete or conflicting routes".into());
        }
        for (target, role) in std::iter::once((body[0], RecipeLoopRouteRole::Enter))
            .chain(successor.map(|id| (id, RecipeLoopRouteRole::Exit)))
        {
            if outgoing
                .iter()
                .filter(|edge| {
                    edge.to == target
                        && edge.event == "on_success"
                        && edge.r#loop.is_none()
                        && edge.loop_route.as_ref().is_some_and(|route| {
                            route.owner_step_id == step.id && route.role == role
                        })
                })
                .count()
                != 1
            {
                return Err("loop route does not match wrapper body/successor topology".into());
            }
        }
        for (from, to, repeat) in
            body.windows(2)
                .map(|pair| (pair[0], pair[1], false))
                .chain(std::iter::once((
                    *body.last().unwrap(),
                    step.id.as_str(),
                    true,
                )))
        {
            let edges: Vec<_> = graph
                .edges
                .iter()
                .filter(|edge| edge.from == from)
                .collect();
            if edges.len() != 1
                || edges[0].to != to
                || edges[0].event != "on_success"
                || edges[0].guard.is_some()
                || edges[0].r#loop.is_some() != repeat
                || edges[0].loop_route.is_some()
            {
                return Err("loop body/repeat topology is inconsistent".into());
            }
        }
    }
    if owners.is_empty()
        || graph
            .edges
            .iter()
            .filter_map(|edge| edge.loop_route.as_ref())
            .any(|route| !owners.contains(route.owner_step_id.as_str()))
    {
        return Err("marked graph requires loop-wrapper owners".into());
    }
    Ok(())
}
