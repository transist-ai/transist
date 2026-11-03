# Guides

Guides are practical, task-oriented walkthroughs for technical evaluators, workflow authors, operators, and platform engineers.

Use them when you want to evaluate Transist, build a workflow, inspect a run, or
operate a local installation without reading the full handbook first.

## Start Here

- Start here:
  [Quickstart](getting-started/quickstart.md) for a temporary standalone evaluation.
- Platform engineer delivering regular controlled workflows:
  [Governed Runtime Setup and Delivery](trust/governed-runtime-setup-and-delivery.md)
- Workflow author:
  [Build Your First Real Workflow](authoring/advanced-workflow-authoring.md)
  for a complete standalone shell-plugin workflow with typed input, a JSON
  output handoff, and a manual gate.
- First-time plugin author:
  [Write Your First Plugin](plugins/write-your-first-plugin.md)
  for a complete local Python APP greeting plugin tutorial.
- Operator inspecting or debugging runs:
  [Inspecting Runs, Artifacts, Evidence, And Events](inspection/inspecting-runs-artifacts-evidence-and-events.md)
- Platform engineer operating a local installation:
  [Operating The Transist Home Directory](inspection/operating-the-transist-home-directory.md)
- Plugin author packaging plugins:
  [Plugin Packaging, Install, Upgrade, And Removal](plugins/plugin-packaging-install-upgrade-removal.md)

## Categories

- [Getting Started](getting-started/README.md)
- [Authoring](authoring/README.md)
- [Inspection](inspection/README.md)
- [Trust](trust/README.md)
- [Plugins](plugins/README.md)

## AI-Assisted Transist Work

If you're using Codex or another approved skill-aware assistant, start with
[AI-Assisted Authoring With Repo Skills](authoring/ai-assisted-authoring-with-repo-skills.md).
It maps the Transist skills in `.agents/skills/` to workflow
authoring, plugin authoring, run recovery, secrets, and observability work.

## Reference Boundary

Guides show the workflow. Reference pages own the exact command, schema,
protocol, and API contract details.

See [Runtime Modes](../reference/runtime-modes.md) before choosing a local
evaluation, governed runtime, or development home.
