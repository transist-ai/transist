# GitHub protection snapshots

These importable files record the live `transist-ai/transist` rulesets. Committing
or merging them does **not** apply settings to GitHub. Each JSON object contains
only editable ruleset fields, without response IDs or timestamps. The separate
[repository settings](../repository-settings.json) snapshot contains selected
repository-level PATCH fields, not a ruleset wrapper.

## Effective protections

- `main-branch-protection` requires PRs, resolved conversations, linear history,
  and the actual GitHub Actions `pr-gate` check against an up-to-date base. It
  blocks deletion and force pushes and permits only final squash merges.
- `linear-pr-branches` requires linear history on non-default branches, excluding
  literal `main`, without blocking rebase force pushes. Rebase PR branches instead
  of merging `main` into them. Publish rewritten history only after authorization,
  using `--force-with-lease`; never force-push `main`.
- The legacy `main-dco-signoff` ruleset is disabled. The merger checks the final
  squash message for a genuine `Signed-off-by:` trailer. The
  [`Main DCO` workflow](../workflows/main-dco.yml) checks new commits after they
  land on `main`; it does not block merges.
- `v-tag-protection` prevents updating, deleting, or force-updating existing `v*`
  tags. It does not block tag creation. No ruleset grants bypass permissions.

Repository settings allow only squash merging, retain commit details in the
default squash message, and do not require web-based commit sign-offs. Those
settings are separate from branch rules. Use each companion repository's actual
CI gate names; do not copy `pr-gate` into a repository that does not produce it.

## Refresh snapshots

From the repository root, with authenticated `gh`, use read-only API requests:

```sh
set -eu
repo=transist-ai/transist
for name in linear-pr-branches main-branch-protection main-dco-signoff v-tag-protection; do
  id="$(gh api "repos/$repo/rulesets" --paginate --jq \
    ".[] | select(.name == \"$name\") | .id")"
  case "$id" in ''|*[!0-9]*) exit 1 ;; esac
  gh api "repos/$repo/rulesets/$id" \
    --jq '{name,target,enforcement,conditions,rules,bypass_actors}' \
    > ".github/rulesets/$name.json"
done
gh api "repos/$repo" \
  --jq '{allow_merge_commit,allow_squash_merge,allow_rebase_merge,allow_auto_merge,allow_update_branch,delete_branch_on_merge,web_commit_signoff_required,squash_merge_commit_title,squash_merge_commit_message}' \
  > .github/repository-settings.json
```

Review the diff and run `bash scripts/ci/check_ci_static.sh`. Do not export the
entire repository response or store credentials in these files.

## Apply a separately authorized change

Only an explicitly authorized administrator should apply reviewed templates.
Resolve the existing ruleset uniquely by name, then update that ID with `PUT`:

```sh
repo=transist-ai/transist
name=main-branch-protection
id="$(gh api "repos/$repo/rulesets" --paginate --jq \
  ".[] | select(.name == \"$name\") | .id")" || exit 1
case "$id" in ''|*[!0-9]*) exit 1 ;; esac
gh api --method PUT "repos/$repo/rulesets/$id" \
  --input ".github/rulesets/$name.json"
```

Repeat the lookup for each intended rule. Stop on missing or ambiguous names;
do not blindly `POST` duplicate rulesets. Repository settings require a separate
operation:

```sh
gh api --method PATCH "repos/$repo" --input .github/repository-settings.json
```

After an authorized change, re-export and compare the live editable fields.
