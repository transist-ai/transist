---
name: transist-pr-preparation
description: Prepare, open, or update a GitHub pull request for transist-ai/transist. Use when an agent needs to review PR scope, fill the repository template, record validation and DCO evidence, or publish an authorized PR with gh.
---

# Transist PR Preparation

Use from a `transist-ai/transist` source checkout. Preparing a PR does not
authorize committing, pushing, or publishing it.

## Default Workflow

1. Read `CONTRIBUTING.md`. If the change touches protected surfaces, also read
   `AGENTS.md` and `.agents/CORE_CHANGE_CHECKLIST.md`. Only trusted contributors
   and maintainers may open implementation PRs upstream; otherwise start with
   an issue.
2. Confirm the target repository, base branch, and head branch. Review the full
   diff against the base and keep unrelated work out. Check whether the head
   already has a PR before opening another one.
3. Run the applicable checks from `CONTRIBUTING.md` after the final change.
   Record the commands and outcomes, including skips, failures, and timeouts.
   Feature-branch sign-offs are not required. Committing and pushing require
   separate authorization.
4. Fill `.github/pull_request_template.md` as described in the
   [PR body and publishing reference](references/pr-body-and-publishing.md).
   If validation is pending, prepare or open a draft and explain what remains.
5. When authorized to push and open or update the PR, use `gh` and verify the
   published URL, head, base, and draft status. Report CI status separately
   from local validation. Opening or preparing a PR does not authorize merging.

The human merger checks the final GitHub squash message for at
least one genuine `Signed-off-by: Name <email>` trailer, preserves genuine
trailers, and adds their own only if needed and they can attest under the DCO.
Never invent a trailer or sign for someone else; bot-authored PRs follow the same
rule. The `main-dco` check runs after merge, not as a PR gate.

## Hard Rules

- Do not claim a check passed unless it did. Failed or skipped applicable checks
  require explicit maintainer approval before merge.
- Do not publish secrets, private logs, or credentials in the PR body. On an
  authentication error, verify the intended account and repository access
  without printing credentials or changing login settings.
- Opening or updating a PR does not authorize merging it, changing repository
  settings, or force-pushing.

## Source Priority

Use `CONTRIBUTING.md` for policy, `.github/pull_request_template.md` for the
body, and `.github/workflows/pr-ci.yml` for hosted enforcement. Follow the
checked-out versions over generic GitHub guidance. If using an extracted
release, switch to a source checkout before preparing an upstream PR.

## References

- [PR body and publishing](references/pr-body-and-publishing.md)
