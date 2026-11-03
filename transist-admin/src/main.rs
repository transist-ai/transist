// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Command-line tooling for creating and verifying Transist trust material.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use transist_admin::{dev, digest, inspect, key, package, sign, verify};
use transist_core::signature_sidecar_path;

#[derive(Debug, Parser)]
#[command(name = "transist-admin", version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Administrative trust tooling for Transist")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create signing keys and export public trust roots.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    /// Package, sign, and verify workflow artifacts.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Sign and verify plugin artifacts.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Inspect detached Transist signatures.
    Signature {
        #[command(subcommand)]
        command: SignatureCommand,
    },
    /// Run development-only maintenance tasks.
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    /// Generate a new Transist administrative signing key.
    Generate {
        #[arg(long)]
        name: String,
        /// Path for a secret `transist.admin.key.v1` JSON file; keep it private.
        #[arg(long)]
        private_key_file: PathBuf,
    },
    /// Export a public trust root from a private signing key.
    ExportPublic {
        /// Path to the secret `transist.admin.key.v1` JSON file to read.
        #[arg(long)]
        private_key_file: PathBuf,
        /// Output path for a public, distributable `transist.trust_root.v1` JSON file.
        #[arg(long)]
        trust_root_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    /// Package and sign an immutable workflow release with its captured assets.
    Package {
        workflow: PathBuf,
        #[arg(long)]
        version: String,
        /// Path to the secret `transist.admin.key.v1` JSON file to use for signing.
        #[arg(long)]
        private_key_file: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Create a detached signature for a workflow.
    Sign {
        workflow: PathBuf,
        /// Path to the secret `transist.admin.key.v1` JSON file to use for signing.
        #[arg(long)]
        private_key_file: PathBuf,
        /// Signature output path. Defaults to `<workflow>.sig`.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Verify a workflow signature against a public trust root.
    Verify {
        workflow: PathBuf,
        #[arg(long)]
        signature: PathBuf,
        /// Path to a public, distributable `transist.trust_root.v1` JSON file.
        #[arg(long)]
        trust_root_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    /// Create a detached signature for a plugin.
    Sign {
        plugin_path: PathBuf,
        /// Path to the secret `transist.admin.key.v1` JSON file to use for signing.
        #[arg(long)]
        private_key_file: PathBuf,
        /// Signature output path. Defaults to `<plugin-directory>.sig`.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Verify a plugin signature against a public trust root.
    Verify {
        plugin_path: PathBuf,
        #[arg(long)]
        signature: PathBuf,
        /// Path to a public, distributable `transist.trust_root.v1` JSON file.
        #[arg(long)]
        trust_root_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum SignatureCommand {
    /// Read and display metadata from a detached signature.
    Inspect { signature: PathBuf },
}

#[derive(Debug, Subcommand)]
enum DevCommand {
    /// Regenerate Transist admin test fixtures.
    RegenerateFixtures {
        #[arg(long)]
        fixture_root: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Key { command } => match command {
            KeyCommand::Generate {
                name,
                private_key_file,
            } => print_json(&key::generate_key(&name, &private_key_file)?),
            KeyCommand::ExportPublic {
                private_key_file,
                trust_root_file,
            } => print_json(&key::export_public_key(
                &private_key_file,
                &trust_root_file,
            )?),
        },
        Command::Workflow { command } => match command {
            WorkflowCommand::Package {
                workflow,
                version,
                private_key_file,
                output,
            } => print_json(&package::package_workflow(
                &workflow,
                &version,
                &private_key_file,
                &output,
            )?),
            WorkflowCommand::Sign {
                workflow,
                private_key_file,
                output,
            } => {
                let output = output.unwrap_or_else(|| signature_sidecar_path(&workflow));
                print_json(&sign::sign_workflow(&workflow, &private_key_file, &output)?)
            }
            WorkflowCommand::Verify {
                workflow,
                signature,
                trust_root_file,
            } => {
                let verification =
                    verify::verify_workflow(&workflow, &signature, &trust_root_file)?;
                print_json(&verification)?;
                if !verification.verified {
                    std::process::exit(1);
                }
                Ok(())
            }
        },
        Command::Plugin { command } => match command {
            PluginCommand::Sign {
                plugin_path,
                private_key_file,
                output,
            } => {
                let output = match output {
                    Some(output) => output,
                    None => signature_sidecar_path(&digest::resolve_plugin_dir(&plugin_path)?),
                };
                print_json(&sign::sign_plugin(
                    &plugin_path,
                    &private_key_file,
                    &output,
                )?)
            }
            PluginCommand::Verify {
                plugin_path,
                signature,
                trust_root_file,
            } => {
                let verification =
                    verify::verify_plugin(&plugin_path, &signature, &trust_root_file)?;
                print_json(&verification)?;
                if !verification.verified {
                    std::process::exit(1);
                }
                Ok(())
            }
        },
        Command::Signature { command } => match command {
            SignatureCommand::Inspect { signature } => {
                print_json(&inspect::inspect_signature(&signature)?)
            }
        },
        Command::Dev { command } => match command {
            DevCommand::RegenerateFixtures { fixture_root } => {
                print_json(&dev::regenerate_fixtures(&fixture_root)?)
            }
        },
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_has_expected_subcommands() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_visible_command_help_explains_its_purpose() {
        let cases = [
            (&[][..], "Administrative trust tooling for Transist"),
            (
                &["key"][..],
                "Create signing keys and export public trust roots.",
            ),
            (
                &["key", "generate"],
                "Generate a new Transist administrative signing key.",
            ),
            (
                &["key", "export-public"],
                "Export a public trust root from a private signing key.",
            ),
            (
                &["workflow"],
                "Package, sign, and verify workflow artifacts.",
            ),
            (
                &["workflow", "package"],
                "Package and sign an immutable workflow release with its captured assets.",
            ),
            (
                &["workflow", "sign"],
                "Create a detached signature for a workflow.",
            ),
            (
                &["workflow", "verify"],
                "Verify a workflow signature against a public trust root.",
            ),
            (&["plugin"], "Sign and verify plugin artifacts."),
            (
                &["plugin", "sign"],
                "Create a detached signature for a plugin.",
            ),
            (
                &["plugin", "verify"],
                "Verify a plugin signature against a public trust root.",
            ),
            (&["signature"], "Inspect detached Transist signatures."),
            (
                &["signature", "inspect"],
                "Read and display metadata from a detached signature.",
            ),
            (&["dev"], "Run development-only maintenance tasks."),
            (
                &["dev", "regenerate-fixtures"],
                "Regenerate Transist admin test fixtures.",
            ),
        ];

        for (path, purpose) in cases {
            let mut command = Cli::command();
            for segment in path {
                command = command
                    .find_subcommand_mut(segment)
                    .unwrap_or_else(|| panic!("missing command path: {}", path.join(" ")))
                    .clone();
            }
            let mut buffer = Vec::new();
            command
                .write_long_help(&mut buffer)
                .expect("render command help");
            let help = String::from_utf8(buffer).expect("help is utf-8");

            assert!(
                help.contains(purpose.trim_end_matches('.')),
                "{} --help should explain its purpose; output:\n{help}",
                path.join(" ")
            );
        }
    }

    #[test]
    fn workflow_verify_parses() {
        let cli = Cli::try_parse_from([
            "transist-admin",
            "workflow",
            "verify",
            "workflow.yaml",
            "--signature",
            "workflow.yaml.sig",
            "--trust-root-file",
            "org.pub",
        ])
        .expect("parse workflow verify");

        match cli.command {
            Command::Workflow {
                command:
                    WorkflowCommand::Verify {
                        workflow,
                        signature,
                        trust_root_file,
                    },
            } => {
                assert_eq!(workflow, PathBuf::from("workflow.yaml"));
                assert_eq!(signature, PathBuf::from("workflow.yaml.sig"));
                assert_eq!(trust_root_file, PathBuf::from("org.pub"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn fixture_regeneration_parses() {
        let cli = Cli::try_parse_from([
            "transist-admin",
            "dev",
            "regenerate-fixtures",
            "--fixture-root",
            "tests/fixtures/admin_tooling",
        ])
        .expect("parse fixture regeneration");

        match cli.command {
            Command::Dev {
                command: DevCommand::RegenerateFixtures { fixture_root },
            } => assert_eq!(fixture_root, PathBuf::from("tests/fixtures/admin_tooling")),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn plugin_help_uses_plugin_path_placeholder() {
        let mut command = Cli::command();
        let plugin = command
            .find_subcommand_mut("plugin")
            .expect("plugin subcommand");
        let sign = plugin
            .find_subcommand_mut("sign")
            .expect("plugin sign subcommand");
        let mut buffer = Vec::new();
        sign.write_long_help(&mut buffer).expect("render help");
        let help = String::from_utf8(buffer).expect("utf8 help");

        assert!(help.contains("<PLUGIN_PATH>"));
        assert!(!help.contains("<PLUGIN_DIR>"));
    }

    #[test]
    fn cryptographic_file_help_is_explicit() {
        let mut command = Cli::command();
        let key = command.find_subcommand_mut("key").expect("key subcommand");
        let generate = key
            .find_subcommand_mut("generate")
            .expect("key generate subcommand");
        let mut generate_help = Vec::new();
        generate
            .write_long_help(&mut generate_help)
            .expect("render key generate help");
        let generate_help = String::from_utf8(generate_help).expect("utf8 help");
        assert!(generate_help.contains("--private-key-file"));
        assert!(generate_help.contains("transist.admin.key.v1"));
        assert!(!generate_help.contains("--output"));

        let mut command = Cli::command();
        let workflow = command
            .find_subcommand_mut("workflow")
            .expect("workflow subcommand");
        let verify = workflow
            .find_subcommand_mut("verify")
            .expect("workflow verify subcommand");
        let mut verify_help = Vec::new();
        verify
            .write_long_help(&mut verify_help)
            .expect("render workflow verify help");
        let verify_help = String::from_utf8(verify_help).expect("utf8 help");
        assert!(verify_help.contains("--trust-root-file"));
        assert!(verify_help.contains("transist.trust_root.v1"));
        let removed_trusted_key = format!("--{}", "trusted-key");
        assert!(!verify_help.contains(&removed_trusted_key));
    }
}
