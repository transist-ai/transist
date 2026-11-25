# PR Body and Publishing

Copy the current `.github/pull_request_template.md` to a temporary Markdown file.
Keep each required `##` heading exactly once. Summarize what changed and why,
link the relevant issue if there is one, and check the applicable classification
and impact options. Add any required core-change record, compatibility notes, or
trust-boundary impact. Fill in Validation, Tests, and Docs with actual evidence
or explain why they do not apply. Template comments do not count as responses,
and a legacy DCO checkbox alone does not count as a validation result. The target
policy does not require a PR-body attestation.

Use the completed file as the body instead of `gh pr create --fill`. For
example, after pushing the intended head branch and confirming it has no PR:

```bash
gh pr create --repo transist-ai/transist --base main --head "$head_branch" \
  --title "$title" --body-file "$body_file" --draft
```

Set `head_branch`, `title`, and `body_file` to the verified values first. Omit
`--draft` only when the change is ready for review. To update an existing PR,
use `gh pr edit` with its verified number and `--body-file`; preserve still
relevant details and evidence from the existing body.

Inspect the result with `gh pr view` and, for a ready PR, check the hosted
results with `gh pr checks`. For ready PRs targeting `main`, an incomplete
body blocks `PR Gate / pr-gate`. Fix the reported sections by editing the
body; a new commit is not needed to rerun CI.
