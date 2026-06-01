use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use azure_support_ticket_mcp::{bootstrap, config::Config, mcp, AppResult};

#[derive(Debug, Parser)]
#[command(
    name = "azure-support-ticket-mcp",
    version,
    about = "MCP server for Azure support tickets"
)]
struct Cli {
    /// Path to config file. Defaults to ~/.azure-support-ticket-mcp/config.toml.
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the MCP server over stdio (default).
    Serve,
    /// Print version and exit.
    Version,
    /// Run lightweight environment / connectivity checks.
    Doctor,
    /// Force re-load of embedded seed and run cache migrations.
    Init,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> ExitCode {
    init_tracing();

    let cli = Cli::parse();
    let cmd = cli.command.unwrap_or(Command::Serve);

    let result: AppResult<()> = async {
        let config = Config::load(cli.config.as_deref())?;
        match cmd {
            Command::Version => {
                println!("azure-support-ticket-mcp {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            Command::Init => {
                let state = bootstrap::ensure_initialized(&config).await?;
                info!(seed_version = %state.seed_version, services = state.services_loaded, "init complete");
                Ok(())
            }
            Command::Doctor => bootstrap::doctor::run(&config).await,
            Command::Serve => {
                let state = bootstrap::ensure_initialized(&config).await?;
                info!(seed_version = %state.seed_version, services = state.services_loaded, "serve starting");
                mcp::serve_stdio(state).await
            }
        }
    }
    .await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!(error = %err, "fatal");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    // Logs go to stderr so MCP stdio JSON-RPC traffic on stdout is never polluted.
    let filter = EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("azure_support_ticket_mcp=info,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .compact()
        .init();
}
