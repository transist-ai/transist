# Admin Tooling Fixtures

The workflow, plugin, public trust root, and detached signature files in this
directory are public verification fixtures. Private signing material is created
in a temporary directory by admin and trust tests and is never stored here.

`transist-admin dev regenerate-fixtures` follows the same rule: it writes only
the public root, detached signatures, and digest metadata into this directory.
