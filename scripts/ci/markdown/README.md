# Markdown CI dependencies

Run repository lint with `bash scripts/ci/check_markdown.sh`. Install the exact
dependency tree with `bash scripts/ci/install_ci_tools.sh markdown`.
The lint entry point runs the focused regression fixtures before repository lint.

The runner uses maintained `markdownlint` directly. All built-in rules use its
micromark parser; no Markdown-it factory or custom rules are needed. JSONC is
parsed with `jsonc-parser`, and `tinyglobby` selects the existing root `globs`
and `ignores` with dotfiles enabled. Selection includes untracked and Git-ignored
files, as the previous CLI2 invocation did. Git enumeration with
`--exclude-standard` would silently narrow that coverage.

The shared `.markdownlint-cli2.jsonc` files retain their existing contents:
root selection and rules, plus inherited nested rule overrides (currently
`tests/fixtures/` disabling MD041). Unsupported configuration formats/options
fail explicitly. This small repository runner is not a general CLI2 replacement.

## Dependency decisions

- `markdownlint`: `0.41.1`, pinned again as `MARKDOWNLINT_VERSION` in
  `scripts/ci/tool-versions.env`.
- `jsonc-parser`: `3.3.1`.
- `tinyglobby`: `0.2.17`, using `picomatch` rather than `micromatch`/`braces`.
- Scoped override: `micromark-extension-math` → `katex 0.18.2`.
  The extension still requests `^0.16.0`, which is affected by
  [GHSA-238p-pmpm-9mq7](https://github.com/advisories/GHSA-238p-pmpm-9mq7).
  The linter only uses math tokenization, but tests also exercise the extension's
  HTML renderer with inline fractions and display square roots against patched
  KaTeX. Remove the override when the extension accepts a patched release.

CLI2 `0.23.3` still pins `micromatch 4.0.8`, which requires `braces ^3.0.3`;
there is no published patched braces release for
[GHSA-vfj7-8cjw-p6xm](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm).
Its other parser dependencies also require audit remediation. The alternative
`markdownlint-cli 0.49.1` avoids braces but introduces YAML/TOML dependencies and
needs configuration translation. Direct library use removes those unnecessary
dependencies without a fork or advisory exception.

## Focused verification

From this directory, run:

```sh
npm ci --ignore-scripts --no-audit
npm test
npm audit --audit-level=low
```

The fixtures cover file selection, exact ignores, nested rule inheritance,
representative existing rule failures, math compatibility, and CLI exit codes.
The committed lockfile pins the complete tree and integrity hashes. The security
workflow's blocking npm audit and Dependabot coverage remain in effect.
