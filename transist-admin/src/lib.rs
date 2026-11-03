// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]

//! Library API for [`transist-admin`](https://transist.ai) trust tooling.
//!
//! Exposes Ed25519 key generation, public-key export, and workflow/plugin
//! signing so that integration tests and other crates can call the
//! trust-creation pipeline in-process instead of spawning the CLI binary.
//!
//! Trust **creation** (key generation, signing) lives here.
//! Trust **enforcement** (verification at runtime) lives in `transist-core`.

/// Development tooling for regenerating test fixtures.
pub mod dev;
/// Computing subject descriptors and content digests for signing and verification.
pub mod digest;
/// Inspecting signature envelopes.
pub mod inspect;
/// Generating and exporting Ed25519 trust keys.
pub mod key;
/// Creating immutable, signed workflow packages.
pub mod package;
/// Signing workflows and plugins with an admin key.
pub mod sign;
/// Loading, saving, and encoding Ed25519 signature artifacts.
pub mod signature;
/// Verifying workflow and plugin signatures against a trusted root.
pub mod verify;

pub use dev::{FixtureRegenerationReport, regenerate_fixtures};
pub use digest::{
    SubjectDescriptor, plugin_subject, resolve_plugin_dir, workflow_subject,
    workflow_subject_from_snapshot,
};
pub use inspect::inspect_signature;
pub use key::{ExportedPublicKeySummary, GeneratedKeySummary, export_public_key, generate_key};
pub use package::{WorkflowPackageResult, package_workflow};
pub use sign::{SignResult, sign_plugin, sign_workflow, sign_workflow_snapshot};
pub use signature::{
    ADMIN_KEY_SCHEMA_V1, AdminKeyRecord, LoadedAdminKey, SignatureInspectView, decode_sha256_hex,
    decode_signature, encode_signature, format_subject_key, inspect_view, kind_label,
    load_admin_key, load_signature_envelope, load_trust_root, save_admin_key,
    save_signature_envelope, save_trust_root, sha256_hex, verifying_key_from_trust_root,
};
pub use verify::{SignatureVerification, verify_plugin, verify_workflow, verify_workflow_snapshot};
