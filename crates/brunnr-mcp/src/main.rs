// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "brunnr-mcp", about = "Brunnr MCP memory server")]
struct Args {
    #[arg(long, env = "BRUNNR_MEMORY_ROOT", default_value = ".brunnr")]
    root: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    brunnr_mcp::run_stdio(args.root).await
}
