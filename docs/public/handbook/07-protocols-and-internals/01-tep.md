# TEP: The Transist Extension Protocol

TEP is the public protocol between the Transist kernel and adapters.

## Why TEP matters

- The kernel can request work without knowing the adapter's transport details.
- Every request and result stays tied to a run and execution context.
- The protocol keeps retries, recovery, and inspection consistent.

## Boundary

TEP sits between orchestration and execution.

1. The kernel sends a request with action, input, paths, and policy metadata.
2. The adapter translates that request into the execution participant's native shape.
3. The adapter returns a structured result with status, output, artifacts, evidence, and usage data when available.

## Related

- [Adapter Responsibilities](../06-architecture-and-extension/04-adapter-responsibilities.md)
- [TEP Technical Reference](../../reference/tep.md)
- [APP: Adapter Plugin Protocol](02-app.md)

### Navigation

- Previous: [Adapter Responsibilities](../06-architecture-and-extension/04-adapter-responsibilities.md)
- Next: [APP](02-app.md)
