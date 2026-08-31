use anyhow::Result;
use clap::Parser;
use metteur_web_server::cli::connect_daemon;
use metteur_web_server::{Cli, build_router, serve};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = Cli::parse();
    let client = connect_daemon(&cli).await?;
    let app = build_router(client, cli.static_dir.clone());
    serve(cli, app).await
}