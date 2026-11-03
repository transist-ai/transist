// SPDX-License-Identifier: MPL-2.0
import { getVersion } from "markdownlint";
import path from "node:path";
import { lintMarkdown } from "./lint.mjs";

try {
  if (getVersion() !== process.argv[2]) {
    throw new Error(
      "Markdown library version mismatch; reinstall locked tools",
    );
  }
  const { files, results } = await lintMarkdown(process.cwd());
  let count = 0;
  for (const result of results) {
    count += Object.values(result).flat().length;
    for (const [file, errors] of Object.entries(result)) {
      for (const error of errors) {
        const column = error.errorRange?.[0] || 1;
        const detail = error.errorDetail ? ` [${error.errorDetail}]` : "";
        console.error(
          `${path.relative(process.cwd(), file)}:${error.lineNumber}:${column} ${error.ruleNames.join("/")} ${error.ruleDescription}${detail}`,
        );
      }
    }
  }
  console.log(
    `markdownlint v${getVersion()}: ${files.length} file(s), ${count} error(s)`,
  );
  process.exitCode = count ? 1 : 0;
} catch (error) {
  console.error(`FAIL: ${error.message}`);
  process.exitCode = 2;
}
