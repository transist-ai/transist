// SPDX-License-Identifier: MPL-2.0
#![cfg_attr(not(clippy), deny(missing_docs))]

//! Binary entry point for the Transist companion HTTP server.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    let args = transist_server::parse_args()?;
    transist_server::run_from_cli(args).await
}
