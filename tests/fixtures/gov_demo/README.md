# Gov Demo Test Fixtures

This directory holds public trust-input fixtures used by the governed-demo
shaped integration tests.

- `test-org.pub`: matching trust root imported into isolated test homes

Gov and trusted-execution tests generate an ephemeral private key in each
temporary test repository, export its public root, and destroy it with the
temporary directory. No private key is required from this directory.

The shaped workflows and stub plugins used by the tests are generated into each
temporary test repository at runtime so they do not appear in the user-facing
workflow or plugin surfaces.
