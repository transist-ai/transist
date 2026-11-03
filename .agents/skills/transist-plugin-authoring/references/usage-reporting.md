# Usage Reporting

APP invoke results expose an optional `usage` object. It is the only source runtimes use to aggregate token data, so plugins that wrap a model-backed tool SHOULD populate it. Plugins that do not touch a model SHOULD leave it absent.

## Canonical Shape

`usage` is a flat JSON object. Use these keys; omit a key when its value is zero or unknown.

Required when any model work happened:

- `input_tokens` (int)
- `output_tokens` (int)
- `total_tokens` (int)

Optional:

- `reasoning_tokens` (int)
- `cache_read_tokens` (int)
- `cache_write_tokens` (int)
- `model` (string, e.g. `openai/gpt-5.1`)
- `provider` (string)

Rules:

- Counts are cumulative for the invocation, not per-step.
- `output_tokens` MUST include `reasoning_tokens` when the backend separates them.
- `total_tokens` MUST equal `input_tokens + output_tokens` unless the backend reports a different total that also covers cache tokens.
- All integer fields MUST be non-negative integers.

## Extraction Patterns

Pick the pattern that matches how the plugin invokes the model-backed tool.
Reference implementations should live with the plugin package being authored.

### 1. Streaming JSON events on stdout

Example: `opencode run --format json` emits newline-delimited JSON with `step_finish` events carrying `part.tokens`. Accumulate across steps.

```json
{"type":"step_finish","sessionID":"…","part":{"type":"step-finish","tokens":{"input":5,"output":6,"reasoning":0,"cache":{"read":0,"write":15405},"total":15416}}}
```

Parse events as they arrive, sum `input`/`output`/`reasoning`/`cache.read`/`cache.write`, and remember the last non-zero `total`. See `opencode.cli/plugin.py` (`accumulate_step_usage`, `finalize_usage`).

### 2. Session-state event files on disk

Example: `gh copilot` writes `~/.copilot/session-state/<session-id>/events.jsonl` with `session.start` and `session.shutdown` events. The shutdown event's `data.modelMetrics.<model>.usage` contains `inputTokens`, `outputTokens`, `reasoningTokens`, `cacheReadTokens`, `cacheWriteTokens`.

Required matching strategy:

- Snapshot directory mtimes before invoking the tool.
- Record an epoch start time.
- After the tool exits, walk sessions whose `events.jsonl` mtime advanced past the snapshot.
- Match by `data.context.cwd` or `data.context.gitRoot` equal to the tool's working directory.
- Prefer the session whose `session.start.timestamp` is closest to (and not earlier than) the recorded start epoch.

See `github.copilot.cli/plugin.py` (`snapshot_copilot_sessions`, `extract_copilot_session_usage`, `_copilot_usage_from_shutdown`). Honor `COPILOT_CONFIG_DIR` and `COPILOT_SESSION_STATE_DIR` overrides.

### 3. Rollout files with token_count events

Example: `codex exec` writes `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`. The last `event_msg` with `payload.type == "token_count"` carries `payload.info.total_token_usage`.

Walk today's and yesterday's folders, keep files whose mtime is ≥ the recorded start epoch, sort by mtime descending, and parse the first that has a `session_meta` header matching your originator. See `codex.cli/plugin.py` (`extract_codex_usage`, `_parse_codex_session_file`).

### 4. Stdout regex fallback

When the tool prints token totals in plain text and no structured source is available, scan stdout for lines matching patterns like `(input|prompt) tokens[: =]\d+`, `(output|completion) tokens[: =]\d+`, and `total tokens[: =]\d+`. See `parse_copilot_stats` in `github.copilot.cli/plugin.py`.

Use this only as a fallback after structured sources have been tried.

## Best-Effort Contract

Usage extraction MUST NOT fail the primary action.

- Wrap the extraction in a broad try/except, return `None` on any failure.
- A missing or malformed session file is not an error.
- Populate `usage.json` as an artifact hint when usage is available, so the value is inspectable in the run directory.
- When no usage is available, return the invoke result with `usage: null` (or omit the field); do not invent zeros.

## Delegation Anti-Pattern

If a plugin shells out to another model-backed CLI (`gh copilot`, `codex`, `opencode`, `goose`, …) as a raw subprocess instead of invoking the sibling plugin through the runtime, the sibling plugin's accumulator does NOT run. The wrapping plugin MUST perform its own usage extraction using the patterns above.

Example: `openspec` invokes `gh copilot` directly. It must snapshot Copilot session state itself and apply pattern 2; otherwise the step reports no usage even though tokens were spent.

## Authoring Checklist

- [ ] Plugin's invoke result declares `usage` whenever the underlying tool produced tokens.
- [ ] Keys match the canonical shape (`input_tokens`, `output_tokens`, `total_tokens`, optional others).
- [ ] Totals are internally consistent (`output_tokens` includes reasoning; `total_tokens` equals input + output unless the backend reports otherwise).
- [ ] Extraction never raises into the primary action path.
- [ ] `usage.json` artifact hint is emitted when usage is available.
- [ ] Smoke test seeds a representative backend fixture (JSON event, session-state file, rollout, or stdout) and asserts the exact `usage` payload on the result.
- [ ] If the plugin wraps another model-backed CLI via raw subprocess, it extracts usage itself rather than relying on the sibling plugin.
