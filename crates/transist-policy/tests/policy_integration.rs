// SPDX-License-Identifier: MPL-2.0
use transist_core::ports::PolicyPort;
use transist_policy::StaticPolicy;
use transist_protocol::KernelValue;

#[test]
fn policy_profiles_return_expected_decisions() {
    let permissive = StaticPolicy::permissive();
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(
        permissive
            .validate_run_start(tmp.path(), "wf")
            .expect("decision")
            .allowed
    );

    let deny = StaticPolicy::deny_all();
    assert!(
        !deny
            .validate_step_execution("run_1", "step_1", &KernelValue::Null)
            .expect("decision")
            .allowed
    );
}
