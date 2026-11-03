# Internals and Boundaries

To keep Transist reliable and extensible, we maintain strict boundaries between different parts of the system. Knowing where these boundaries lie helps you understand where to add new features and how to avoid "dirty" hacks that compromise the system's integrity.

## Where the Brain Lives: Kernel & Compiler

The logic of a workflow—its shape, its rules, and its memory—lives in the **Kernel** and the **Compiler**. This is where we handle:

- **Graph Construction**: Turning your YAML workflow into a logical execution graph.
- **Validation**: Ensuring that every step has the inputs it needs before the run starts.
- **Control Flow**: Managing loops, guards (conditionals), and parallel execution.
- **Recovery**: Knowing exactly how to resume a interrupted run without losing state.
- **State Transitions**: The source of truth for whether a run is `pending`, `active`, `paused`, or `completed`.

By keeping these semantics in the center, we ensure that workflows behave identically regardless of which plugins or adapters they use.

## Where the Power Lives: Extensibility Seams

Transist is designed to be extended at specific "seams." If you want to change how the system behaves, you should look at these three areas:

1. **Plugins**: Add new tools, LLM prompts, or domain-specific logic.
2. **Adapters**: Add support for new execution environments (like a custom cloud provider or a new protocol).
3. **Policy**: Change the rules for security, secret access, and human-in-the-loop approvals.

## The "Golden Rules" of Boundaries

To maintain the Transist boundary model, Transist maintainers, platform engineers, and approved extension authors must respect these boundaries:

- **Policy is Central**: Security rules and secret redaction should never be "sprinkled" throughout the code. They belong in the central Policy engine.
- **No Tool-Specific Hacks**: The Kernel should never know about specific tools (like "Git" or "Docker"). It only knows about generic Plugins and Actions.
- **Read-Only Means Read-Only**: The Transist Server provides visibility and history. It should never contain mutable logic that changes the outcome of a run—that’s the Kernel’s job.
- **Zero-Secret Persistence**: Adapters must never save secrets to disk. They are "just-in-time" delivery agents only.

Respecting these boundaries keeps Transist's execution model predictable, auditable, and maintainable.

---

## Related

- [TEP: Transist Execution Protocol](01-tep.md)
- [APP: Adapter Plugin Protocol](02-app.md)
- [Storage Layout](../../public/reference/storage-layout.md)
- [Trust & Policy](../../public/guides/trust/README.md)

---

- Previous: [APP](02-app.md)
- Next: [Reference](../../public/reference/README.md)
