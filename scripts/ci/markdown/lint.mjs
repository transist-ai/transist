// SPDX-License-Identifier: MPL-2.0
import { readFile } from "node:fs/promises";
import path from "node:path";
import { parse, printParseErrorCode } from "jsonc-parser";
import { lint } from "markdownlint/promise";
import { glob } from "tinyglobby";

const configName = ".markdownlint-cli2.jsonc";

/**
 * Read the repository's JSONC options without silently accepting unsupported CLI
 * features. Nested files may override rule configuration; selection is root-owned.
 * @param {string} filename Absolute configuration path.
 * @param {boolean} root Whether this is the root configuration.
 * @returns {Promise<object>} Parsed options; throws on malformed/unsupported input.
 */
async function readOptions(filename, root) {
  const errors = [];
  const options = parse(await readFile(filename, "utf8"), errors, {
    allowTrailingComma: true,
  });
  if (errors.length) {
    throw new Error(`${filename}: ${printParseErrorCode(errors[0].error)}`);
  }
  const allowed = root ? ["config", "globs", "ignores"] : ["config"];
  if (
    !options ||
    typeof options !== "object" ||
    Array.isArray(options) ||
    Object.keys(options).some((key) => !allowed.includes(key))
  ) {
    throw new Error(`${filename}: unsupported Markdown options`);
  }
  if (
    !options.config ||
    typeof options.config !== "object" ||
    Array.isArray(options.config) ||
    Object.hasOwn(options.config, "extends")
  ) {
    throw new Error(`${filename}: expected inline rule configuration`);
  }
  for (const key of root ? ["globs", "ignores"] : []) {
    if (
      !Array.isArray(options[key]) ||
      options[key].some((value) => typeof value !== "string")
    ) {
      throw new Error(`${filename}: expected ${key} string array`);
    }
  }
  return options;
}

/**
 * Lint the root's Markdown files with inherited JSONC rules. Includes dotfiles,
 * untracked and Git-ignored files, just like the previous CLI2 globs. Reads only;
 * no fixes, Git/index mutation, configuration imports, or generated outputs.
 * @param {string} root Absolute directory containing the root JSONC configuration.
 * @returns {Promise<{files: string[], results: object[]}>} Sorted files and results.
 */
export async function lintMarkdown(root) {
  const options = await readOptions(path.join(root, configName), true);
  const globOptions = {
    cwd: root,
    dot: true,
    onlyFiles: true,
    expandDirectories: false,
    ignore: options.ignores,
  };
  const files = (await glob(options.globs, globOptions)).sort();
  if (!files.length) {
    throw new Error("No Markdown files selected; refusing an empty lint check");
  }
  const configurations = new Map([["", options.config]]);
  const configFiles = (await glob("**/.markdownlint*", globOptions)).sort(
    (a, b) => a.split("/").length - b.split("/").length || a.localeCompare(b),
  );
  for (const file of configFiles) {
    if (file === configName) continue;
    if (path.posix.basename(file) !== configName) {
      throw new Error(`${file}: unsupported Markdown configuration format`);
    }
    const directory = path.posix.dirname(file);
    const parent = [...configurations.keys()]
      .filter(
        (candidate) => !candidate || directory.startsWith(`${candidate}/`),
      )
      .at(-1);
    const nested = await readOptions(path.join(root, file), false);
    configurations.set(directory, {
      ...configurations.get(parent),
      ...nested.config,
    });
  }
  const groups = new Map();
  for (const file of files) {
    const directory = [...configurations.keys()]
      .filter((candidate) => !candidate || file.startsWith(`${candidate}/`))
      .at(-1);
    if (!groups.has(directory)) groups.set(directory, []);
    groups.get(directory).push(path.join(root, file));
  }
  const results = [];
  for (const [directory, group] of groups) {
    results.push(
      await lint({ files: group, config: configurations.get(directory) }),
    );
  }
  return { files, results };
}
