# APP: The Adapter Plugin Protocol

APP is the public protocol between adapters and plugins.

## Why APP matters

- The adapter can manage different plugin transports without changing kernel behavior.
- Plugin work remains inspectable through a consistent adapter contract.
- Execution details stay behind the adapter boundary.

## Responsibilities

1. The adapter passes structured input to the plugin.
2. The plugin performs the requested action.
3. The plugin returns status, output, and any artifact or evidence hints.

## Related

- [TEP: Transist Extension Protocol](01-tep.md)
- [Adapter Responsibilities](../06-architecture-and-extension/04-adapter-responsibilities.md)
- [APP Technical Reference](../../reference/app.md)

### Navigation

- Previous: [TEP](01-tep.md)
- Next: [Internals And Boundaries](03-internals-and-boundaries.md)
