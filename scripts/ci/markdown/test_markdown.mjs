// SPDX-License-Identifier: MPL-2.0
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { micromark } from "micromark";
import { math, mathHtml } from "micromark-extension-math";
import katex from "katex";
import { lintMarkdown } from "./lint.mjs";

const rootConfig = new URL(
  "../../../.markdownlint-cli2.jsonc",
  import.meta.url,
);
const nestedConfig = new URL(
  "../../../tests/fixtures/.markdownlint-cli2.jsonc",
  import.meta.url,
);
const runner = fileURLToPath(new URL("run.mjs", import.meta.url));
const valid = "# Fixture\n\nValid text.\n";

/**
 * Create an isolated tree with the real repository rules and register cleanup.
 * @param {import('node:test').TestContext} t Test context owning the tree.
 * @param {Record<string, string>} entries Relative file paths and contents.
 * @returns {Promise<string>} Absolute fixture root.
 */
async function fixture(t, entries) {
  const root = await mkdtemp(
    path.join(tmpdir(), "ci-blocker-markdown-fixture-"),
  );
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(
    path.join(root, ".markdownlint-cli2.jsonc"),
    await readFile(rootConfig),
  );
  for (const [file, content] of Object.entries(entries)) {
    const filename = path.join(root, file);
    await mkdir(path.dirname(filename), { recursive: true });
    await writeFile(filename, content);
  }
  return root;
}

/**
 * Collect the rule names reported by the maintained library.
 * @param {object[]} results Library results, grouped by inherited configuration.
 * @returns {Set<string>} Reported rule IDs.
 */
function rules(results) {
  return new Set(
    results.flatMap((result) =>
      Object.values(result).flatMap((errors) =>
        errors.map((error) => error.ruleNames[0]),
      ),
    ),
  );
}

test("selection preserves both extensions, dot trees, special names and exact ignores", async (t) => {
  const included = [
    "README.md",
    "docs/deep/guide.markdown",
    ".agents/skills/example/SKILL.md",
    ".hidden/.guide.md",
    "docs/space [literal]*.md",
    "docs/line\nbreak.md",
    "docs/target/kept.md",
    "docs/.tmp/kept.md",
    "draft.md",
    ".agents/testing/e2e_suites/detached-assets/prompts/neighbor.md",
  ];
  const excluded = [
    "target/bad.md",
    ".tmp/ci-inputs/snapshot/README.md",
    "node_modules/bad.md",
    "docs/node_modules/bad.markdown",
    ".agents/testing/e2e_suites/detached-assets/prompts/task.md",
    "docs/not-markdown.txt",
    "docs/uppercase.MD",
  ];
  const entries = Object.fromEntries(included.map((file) => [file, valid]));
  for (const file of excluded) entries[file] = "bad  \n";
  // Ignored generated/dependency trees must not supply rule configuration either.
  entries["target/.markdownlint.yaml"] = "default: false\n";
  entries[".tmp/ci-inputs/snapshot/.markdownlint-cli2.jsonc"] = "{ broken";
  entries["docs/node_modules/.markdownlint-cli2.jsonc"] = "{ broken";
  entries[".gitignore"] = "draft.md\n";
  const root = await fixture(t, entries);
  const { files, results } = await lintMarkdown(root);
  assert.deepEqual(files, included.sort());
  assert.deepEqual([...rules(results)], []);
});

test("JSONC comments and trailing commas retain root and nested rule settings", async (t) => {
  const root = await fixture(t, {
    ".markdownlint-cli2.jsonc": `{
      // Keep the repository rules while exercising JSONC syntax.
      "globs": ["**/*.md", "**/*.markdown",],
      "ignores": [],
      "config": {"default": true, "MD013": false, "MD024": {"siblings_only": true,}, "MD060": false,},
    }`,
    "nested/.markdownlint-cli2.jsonc": `{
      /* Only the initial-heading rule is disabled in this subtree. */
      "config": {"MD041": false,},
    }`,
    "nested/allowed.markdown": "No heading.\n",
    "nested/bad.md": "Trailing space. \n",
    "root.md": "No heading.\n",
  });
  const { results } = await lintMarkdown(root);
  const byFile = Object.assign({}, ...results);
  assert.deepEqual(byFile[path.join(root, "nested/allowed.markdown")], []);
  assert.deepEqual(
    byFile[path.join(root, "nested/bad.md")].map((error) => error.ruleNames[0]),
    ["MD009"],
  );
  assert.deepEqual(
    byFile[path.join(root, "root.md")].map((error) => error.ruleNames[0]),
    ["MD041"],
  );
});

test("nested fixture MD041 override inherits every root rule and stays scoped", async (t) => {
  const root = await fixture(t, {
    "root.md": "No heading.\n",
    "tests/fixtures/.markdownlint-cli2.jsonc": await readFile(
      nestedConfig,
      "utf8",
    ),
    "tests/fixtures/allowed.md": "No heading.\n",
    "tests/fixtures/deep/bad.md": "No heading. \n",
    "tests/fixtures-neighbor/bad.md": "No heading.\n",
  });
  const { results } = await lintMarkdown(root);
  const byFile = Object.assign({}, ...results);
  assert.deepEqual(byFile[path.join(root, "tests/fixtures/allowed.md")], []);
  assert.equal(byFile[path.join(root, "root.md")][0].ruleNames[0], "MD041");
  assert.equal(
    byFile[path.join(root, "tests/fixtures-neighbor/bad.md")][0].ruleNames[0],
    "MD041",
  );
  assert.deepEqual(
    byFile[path.join(root, "tests/fixtures/deep/bad.md")].map(
      (error) => error.ruleNames[0],
    ),
    ["MD009"],
  );
});

for (const [rule, content] of Object.entries({
  MD001: "# Heading\n\n### Skipped level\n",
  MD009: "# Heading\n\nTrailing space. \n",
  MD022: "# Heading\nMissing blank line.\n",
  MD024: "# Heading\n\n## Duplicate\n\nText.\n\n## Duplicate\n",
  MD031: "# Heading\n\nText.\n```text\nCode.\n```\n",
  MD033: "# Heading\n\n<div>HTML</div>\n",
  MD040: "# Heading\n\n```\nCode.\n```\n",
  MD041: "No initial heading.\n",
  MD047: "# Heading\n\nMissing final newline.",
  MD051: "# Heading\n\n[Broken fragment](#absent)\n",
})) {
  test(`existing ${rule} violations still fail`, async (t) => {
    const root = await fixture(t, { "bad.md": content });
    const { results } = await lintMarkdown(root);
    assert.ok(rules(results).has(rule));
  });
}

test("existing MD013, MD024 siblings_only and MD060 settings remain effective", async (t) => {
  const root = await fixture(t, {
    "valid.md": `# Heading\n\n${"Long text. ".repeat(30).trim()}\n\n## One\n\n### Repeated\n\n## Two\n\n### Repeated\n\n| A | B |\n| - | - |\n| Long | Short |\n`,
  });
  assert.deepEqual([...rules((await lintMarkdown(root)).results)], []);
});

test("patched KaTeX retains math tokenization and the extension's HTML API", async (t) => {
  assert.equal(katex.version, "0.18.2");
  const source = "Inline $a_1 + \\frac{1}{2}$.\n\n$$\n\\sqrt{x^2 + y^2}\n$$\n";
  const root = await fixture(t, { "math.md": `# Math\n\n${source}` });
  assert.deepEqual([...rules((await lintMarkdown(root)).results)], []);
  const html = micromark(source, {
    extensions: [math()],
    htmlExtensions: [mathHtml()],
  });
  assert.match(html, /class="katex"/);
  assert.match(html, /class="mfrac"/);
  assert.match(html, /class="math math-display"/);
});

test("CLI exits distinguish success, rule failures, invalid config and pin mismatch", async (t) => {
  const root = await fixture(t, { "README.md": valid });
  const run = (version = "0.41.1") =>
    spawnSync(process.execPath, [runner, version], {
      cwd: root,
      encoding: "utf8",
    });
  assert.equal(run().status, 0);
  await writeFile(path.join(root, "README.md"), "No heading.\n");
  const failed = run();
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /MD041/);
  assert.equal(run("0.19.0").status, 2);
  await writeFile(path.join(root, ".markdownlint-cli2.jsonc"), "{ broken");
  const malformed = run();
  assert.equal(malformed.status, 2);
  assert.match(malformed.stderr, /FAIL:/);
});

test("empty selection and unsupported configuration fail closed", async (t) => {
  const root = await fixture(t, {});
  await assert.rejects(lintMarkdown(root), /No Markdown files selected/);
  await writeFile(path.join(root, "README.md"), valid);
  await writeFile(path.join(root, ".markdownlint.yaml"), "default: false\n");
  await assert.rejects(
    lintMarkdown(root),
    /unsupported Markdown configuration format/,
  );
  await rm(path.join(root, ".markdownlint.yaml"));
  await writeFile(
    path.join(root, ".markdownlint-cli2.jsonc"),
    '{"config": {}, "fix": true}',
  );
  await assert.rejects(lintMarkdown(root), /unsupported Markdown options/);
});
