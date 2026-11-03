# Workflow Authorization E2E Plan

1. Check authored source, package and sign it outside the runtime, then install
   the signed package without inputs.
2. Check two unrelated repository inputs. Both checks must be blocked and
   preparation must create no run storage.
3. Authorize repository A, recheck it, and prove its exact grant does not make
   repository B ready.
4. Authorize and recheck repository B. Verify two separate policy fragments
   contain only the exact repository paths, never their common parent.
5. Prepare and start one persisted run for each ready repository.
