use clap::Parser;
use ruwa_gateway::config::{Cli, Command};

#[global_allocator]
static ALLOC: ruwa_gateway::sysmetrics::CountingAlloc = ruwa_gateway::sysmetrics::CountingAlloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ruwa_gateway::sysmetrics::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!("ruwa-gateway {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Rest(cfg) => {
            ruwa_gateway::telemetry::init(cfg.debug);
            ruwa_gateway::server::run(*cfg).await
        }
    }
}
