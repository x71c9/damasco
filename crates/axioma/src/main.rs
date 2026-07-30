//! axioma: self-hosted platform that builds and maintains a website through a
//! chat with a coding agent.

mod config;
mod db;
mod http;

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use crate::config::Config;

#[derive(Parser)]
#[command(
    version,
    about = "Self-hosted website builder driven by a coding agent"
)]
struct Args {
    /// Configuration file.
    #[arg(long, env = "AXIOMA_CONFIG", default_value = "/etc/axioma/axioma.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("axioma: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let config = Config::load(&args.config)?;
    let data_dir = config.data_dir.clone();
    let (_db, opened) = tokio::task::spawn_blocking(move || db::open(&data_dir)).await??;
    if let Some(backup) = &opened.backup {
        eprintln!(
            "axioma: database copied to {} before upgrading",
            backup.display()
        );
    }
    eprintln!(
        "axioma: database {} (schema v{})",
        opened.path.display(),
        opened.version
    );
    let addr = config.listen;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| format!("cannot listen on {addr}: {err}"))?;
    eprintln!("axioma: listening on http://{}", listener.local_addr()?);
    eprintln!("axioma: panel   {}", config.hosts.panel);
    eprintln!("axioma: preview {}", config.hosts.preview);
    eprintln!("axioma: site    {}", config.hosts.site.join(", "));
    axum::serve(listener, http::router(config))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    eprintln!("axioma: stopped");
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM, so that systemd stops the service cleanly.
async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}
