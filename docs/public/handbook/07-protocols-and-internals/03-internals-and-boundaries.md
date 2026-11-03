# Internals and Boundaries

Transist stays reliable by keeping clear boundaries between the runtime kernel, adapters, plugins, and external tools.

## Boundaries

- The kernel owns workflow semantics, run state, gates, recovery, and replay.
- Adapters mediate transport and hosting.
- Plugins package tool behavior and action schemas.
- External tools remain outside Transist.

## Why this matters

- The kernel stays small.
- Tool-specific behavior stays in extensions.
- Inspection and recovery stay consistent across transports.

## Related

- [TEP: Transist Extension Protocol](01-tep.md)
- [APP: Adapter Plugin Protocol](02-app.md)
- [Storage Layout](../../reference/storage-layout.md)
- [Compatibility and Stability](../../reference/compatibility-and-stability.md)

### Navigation

- Previous: [APP](02-app.md)
- Next: [Reference](../../reference/README.md)
