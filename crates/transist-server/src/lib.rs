// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]
//! Companion HTTP server for Transist inspection and metrics APIs.
//!
//! This crate owns the read-only server-facing service layer, HTTP transport, and
//! domain DTOs used by the companion API. It may depend on lower-level runtime and
//! storage query crates, but it must not own core runtime execution semantics.

use anyhow::{Context, Result, bail};
use std::net::IpAddr;
use std::path::PathBuf;

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use infrastructure::{
    ApiRoute, COMPANION_API_ROUTES, INSPECTION_GET_ROUTES, ServerConfig, app_for_paths,
    app_with_query, app_with_services, run_server,
};

#[derive(Debug)]
enum ParsedArgs {
    Run(CliArgs),
    Help,
    Version,
}

#[derive(Debug)]
/// Parsed command-line arguments for starting the companion server.
///
/// These values affect only local server bootstrapping. They do not change the
/// runtime semantics of Transist workflows or persisted data.
pub struct CliArgs {
    /// Optional workspace root used to resolve workflows and local defaults.
    pub repo_root: Option<PathBuf>,
    /// Optional explicit Transist home directory override for this process.
    pub home: Option<PathBuf>,
    /// IP address that the HTTP server should bind to.
    pub bind_ip: IpAddr,
    /// TCP port that the HTTP server should listen on.
    pub port: u16,
    /// Opt-in required before binding to a non-loopback address.
    pub allow_non_local_bind: bool,
}

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            repo_root: None,
            home: None,
            bind_ip: "127.0.0.1".parse().expect("default bind ip"),
            port: 7777,
            allow_non_local_bind: false,
        }
    }
}

/// Runs the server from parsed CLI arguments.
pub async fn run_from_cli(args: CliArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("failed resolving current directory")?;
    let workspace_root = resolve_workspace_root(&cwd, args.repo_root);
    if !args.bind_ip.is_loopback() && !args.allow_non_local_bind {
        bail!(
            "refusing non-local bind '{}': pass --allow-non-local-bind to opt in explicitly",
            args.bind_ip
        );
    }
    let resolved_home = match transist::resolve_home(&workspace_root, &cwd, args.home.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            if e.downcast_ref::<transist::HomeResolutionError>().is_some() {
                bail!(
                    "cannot start transist-server: {e}\n\n\
                     Run `transist home init` to initialize the per-user default home, \
                     or `transist --home <path> home init` to initialize an explicit home, \
                     then restart the server."
                );
            }
            return Err(e);
        }
    };
    transist::validate_local_installation(&resolved_home.effective_home).map_err(|e| {
        anyhow::anyhow!(
            "cannot start transist-server: {e}\n\n\
                 Run `transist home init` to initialize the per-user default home, \
                 or `transist --home <path> home init` to initialize an explicit home, \
                 then restart the server."
        )
    })?;

    let mut config = ServerConfig::with_paths(workspace_root, resolved_home.effective_home);
    config.bind_ip = args.bind_ip;
    config.port = args.port;

    run_server(config).await
}

/// Parses process arguments for the `transist-server` binary.
pub fn parse_args() -> Result<CliArgs> {
    match parse_args_from(std::env::args().skip(1))? {
        ParsedArgs::Run(args) => Ok(args),
        ParsedArgs::Help => {
            print_help();
            std::process::exit(0);
        }
        ParsedArgs::Version => {
            println!("transist-server {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
    }
}

fn resolve_workspace_root(cwd: &std::path::Path, workspace_root: Option<PathBuf>) -> PathBuf {
    let raw_workspace_root = workspace_root.unwrap_or_else(|| cwd.to_path_buf());
    if raw_workspace_root.is_absolute() {
        raw_workspace_root
    } else {
        cwd.join(raw_workspace_root)
    }
}

fn parse_args_from<I, S>(args: I) -> Result<ParsedArgs>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut parsed = CliArgs::default();
    let mut args = args.into_iter().map(Into::into);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace-root" | "--repo-root" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--workspace-root requires a path argument"))?;
                parsed.repo_root = Some(PathBuf::from(value));
            }
            "--home" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--home requires a path argument"))?;
                parsed.home = Some(PathBuf::from(value));
            }
            "--bind" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--bind requires an IP address"))?;
                parsed.bind_ip = value.parse::<IpAddr>().with_context(|| {
                    format!("invalid bind ip '{}': expected an IP address", value)
                })?;
            }
            "--port" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--port requires a numeric value"))?;
                parsed.port = value
                    .parse::<u16>()
                    .with_context(|| format!("invalid port '{}': expected 1-65535", value))?;
            }
            "--allow-non-local-bind" => {
                parsed.allow_non_local_bind = true;
            }
            "--help" | "-h" => {
                return Ok(ParsedArgs::Help);
            }
            "--version" | "-V" => {
                return Ok(ParsedArgs::Version);
            }
            _ => {
                bail!(
                    "unknown argument '{}'. Use --help for usage.\n\n{}",
                    arg,
                    usage()
                );
            }
        }
    }

    Ok(ParsedArgs::Run(parsed))
}

/// Prints CLI help text to stdout.
pub fn print_help() {
    println!("{}", usage());
}

/// Returns static CLI usage help text.
pub fn usage() -> &'static str {
    usage_text()
}

fn usage_text() -> &'static str {
    "transist-server\n\nUsage:\n  transist-server [--workspace-root <path>] [--home <path>] [--bind <ip>] [--port <u16>] [--allow-non-local-bind]\n\nRead-only companion inspection API.\nExposes Transist state for local inspection and integration only; execution and control stay in the CLI.\n\nOptions:\n  --workspace-root <path>   Workspace root used to resolve workflows and local defaults (default: current directory)\n  --home <path>             Override the effective Transist home for this server process\n  --bind <ip>               Bind IP (default: 127.0.0.1)\n  --port <u16>              TCP port (default: 7777)\n  --allow-non-local-bind    Required when binding to a non-loopback address\n  -h, --help                Show this help\n  -V, --version             Show version\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_args_uses_defaults() {
        let parsed = parse_args_from(std::iter::empty::<&str>()).expect("parse");
        let ParsedArgs::Run(args) = parsed else {
            panic!("expected run args");
        };
        assert_eq!(args.repo_root, None);
        assert_eq!(args.home, None);
        assert_eq!(args.bind_ip, "127.0.0.1".parse::<IpAddr>().expect("ip"));
        assert_eq!(args.port, 7777);
        assert!(!args.allow_non_local_bind);
    }

    #[test]
    fn parse_args_parses_all_supported_flags() {
        let parsed = parse_args_from([
            "--workspace-root",
            "repo",
            "--home",
            "/tmp/transist-home",
            "--bind",
            "0.0.0.0",
            "--port",
            "9000",
            "--allow-non-local-bind",
        ])
        .expect("parse");
        let ParsedArgs::Run(args) = parsed else {
            panic!("expected run args");
        };
        assert_eq!(args.repo_root, Some(PathBuf::from("repo")));
        assert_eq!(args.home, Some(PathBuf::from("/tmp/transist-home")));
        assert_eq!(args.bind_ip, "0.0.0.0".parse::<IpAddr>().expect("ip"));
        assert_eq!(args.port, 9000);
        assert!(args.allow_non_local_bind);
    }

    #[test]
    fn parse_args_accepts_repo_root_alias() {
        let parsed = parse_args_from(["--repo-root", "repo"]).expect("parse");
        let ParsedArgs::Run(args) = parsed else {
            panic!("expected run args");
        };
        assert_eq!(args.repo_root, Some(PathBuf::from("repo")));
    }

    #[test]
    fn parse_args_returns_help_variant() {
        let parsed = parse_args_from(["--help"]).expect("parse");
        assert!(matches!(parsed, ParsedArgs::Help));
    }

    #[test]
    fn parse_args_returns_version_variant() {
        let parsed = parse_args_from(["--version"]).expect("parse");
        assert!(matches!(parsed, ParsedArgs::Version));
    }

    #[test]
    fn parse_args_returns_version_variant_short() {
        let parsed = parse_args_from(["-V"]).expect("parse");
        assert!(matches!(parsed, ParsedArgs::Version));
    }

    #[test]
    fn parse_args_rejects_invalid_bind_ip() {
        let err = parse_args_from(["--bind", "localhost"]).expect_err("invalid bind");
        assert!(err.to_string().contains("invalid bind ip 'localhost'"));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let err = parse_args_from(["--wat"]).expect_err("unknown arg");
        assert!(err.to_string().contains("unknown argument '--wat'"));
    }

    #[test]
    fn resolve_workspace_root_expands_relative_paths() {
        let cwd = Path::new("/tmp/workspace");
        let resolved = resolve_workspace_root(cwd, Some(PathBuf::from("repo")));
        assert_eq!(resolved, cwd.join("repo"));
    }

    #[test]
    fn resolve_workspace_root_keeps_absolute_paths() {
        let cwd = Path::new("/tmp/workspace");
        let resolved = resolve_workspace_root(cwd, Some(PathBuf::from("/tmp/other")));
        assert_eq!(resolved, PathBuf::from("/tmp/other"));
    }

    #[test]
    fn usage_mentions_expected_flags() {
        let help = usage();
        assert!(help.contains("--workspace-root <path>"));
        assert!(help.contains("--home <path>"));
        assert!(help.contains("--bind <ip>"));
        assert!(help.contains("--port <u16>"));
        assert!(help.contains("--allow-non-local-bind"));
        assert!(!help.contains("--repo-root <path>"));
    }

    #[test]
    fn print_help_is_callable() {
        print_help();
    }
}
