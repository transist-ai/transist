// SPDX-License-Identifier: MPL-2.0

use std::fs;
use tempfile::tempdir;
use transist_admin::workflow_subject;

#[test]
fn workflow_subject_uses_the_canonical_asset_capture() {
    let root = tempdir().expect("tempdir");
    let workflow = root.path().join("workflow.yaml");
    fs::write(
        &workflow,
        "schema: transist.flow.v1\nid: signed\nversion: v1\nplugins:\n  shell: 0.1.0\nsteps:\n  - id: shell\n    uses: process:shell/exec\n    with:\n      command: source://assets/child.sh\n",
    )
    .expect("workflow");
    fs::create_dir(root.path().join("assets")).expect("assets");
    fs::write(root.path().join("assets/child.sh"), "#!/bin/sh\nexit 0\n").expect("asset");

    let first = workflow_subject(&workflow).expect("first subject");
    fs::write(
        root.path().join("assets/child.sh"),
        "#!/bin/sh\nprintf changed\n",
    )
    .expect("changed asset");
    let second = workflow_subject(&workflow).expect("second subject");

    assert_eq!(first.id, "signed");
    assert_eq!(first.version, "v1");
    assert_ne!(first.digest, second.digest);
}
