//! axioma: self-hosted platform that builds and maintains a website through a
//! chat with a coding agent.

mod access;
mod activity;
mod check;
mod config;
mod db;
mod http;
mod limit;
mod panel;
mod people;
mod session;
mod token;

use std::error::Error;
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::check::Level;
use crate::config::Config;

#[derive(Parser)]
#[command(
    version,
    about = "Self-hosted website builder driven by a coding agent"
)]
struct Args {
    /// Configuration file.
    #[arg(
        long,
        global = true,
        env = "AXIOMA_CONFIG",
        default_value = "/etc/axioma/axioma.toml"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the panel, the preview and the site (default).
    Serve,
    /// Check whether this machine can run the agent's sandbox.
    Check,
    /// Print a one-time link to create the owner, or to reset the owner's
    /// password when the owner exists.
    OwnerLink,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let result = match args.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(&args.config).await,
        Command::Check => return check().await,
        Command::OwnerLink => owner_link(&args.config).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("axioma: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Prints every check; fails when any check fails.
async fn check() -> ExitCode {
    let items = run_checks().await;
    print!("{}", check::report(&items));
    if items.iter().any(|item| item.level == Level::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn run_checks() -> Vec<check::Item> {
    tokio::task::spawn_blocking(|| check::run(&check::System::current()))
        .await
        .unwrap_or_default()
}

async fn serve(config_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let config = Config::load(config_path)?;

    let items = run_checks().await;
    let failed = items.iter().filter(|i| i.level != Level::Ok).count();
    if failed == 0 {
        eprintln!("axioma: environment ready for the sandbox");
    } else {
        for item in items.iter().filter(|i| i.level != Level::Ok) {
            eprintln!("axioma: {} {}: {}", item.level, item.name, item.detail);
        }
        eprintln!("axioma: the sandbox stays disabled until `axioma check` passes");
    }

    let data_dir = config.data_dir.clone();
    let (conn, opened) = tokio::task::spawn_blocking(move || db::open(&data_dir)).await??;
    let db = db::Handle::new(conn);
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

    // A new installation prints the link that creates the owner, so that the
    // account is created in the panel by whoever can read this log. Every
    // start prints a new link until the owner exists; earlier ones stay valid.
    let has_owner = db.run(people::has_owner).await?;
    if !has_owner {
        let (token, _) = db.run(|conn| people::owner_link(conn, false)).await?;
        eprintln!();
        eprintln!("axioma: no owner yet; create the owner's account with this one-time link:");
        eprintln!();
        eprintln!("    {}/setup#{token}", config.panel_url());
        eprintln!();
        eprintln!(
            "axioma: it works once, for 24 hours; links printed by earlier starts still work"
        );
        eprintln!();
    }

    let listener = match activated_listener()? {
        Some(listener) => {
            eprintln!("axioma: using the socket passed by systemd; `listen` is ignored");
            tokio::net::TcpListener::from_std(listener)?
        }
        None => {
            let addr = config.listen;
            tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|err| format!("cannot listen on {addr}: {err}"))?
        }
    };
    eprintln!("axioma: listening on http://{}", listener.local_addr()?);
    eprintln!("axioma: panel   {}", config.hosts.panel);
    eprintln!("axioma: preview {}", config.hosts.preview);
    eprintln!("axioma: site    {}", config.hosts.site.join(", "));
    let app =
        http::router(config, db).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    eprintln!("axioma: stopped");
    Ok(())
}

/// The listening socket passed by systemd socket activation, if any. Holding
/// the socket in systemd keeps connections waiting, instead of refused,
/// while the service restarts.
fn activated_listener() -> Result<Option<std::net::TcpListener>, Box<dyn Error>> {
    let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u32>().ok());
    if number("LISTEN_PID") != Some(std::process::id()) {
        return Ok(None);
    }
    match number("LISTEN_FDS") {
        None | Some(0) => Ok(None),
        Some(1) => {
            // SAFETY: with LISTEN_PID naming this process, systemd guarantees
            // that file descriptor 3 is the one socket it passed, open and
            // owned by nobody else in this process.
            let listener = unsafe { std::net::TcpListener::from_raw_fd(3) };
            listener.set_nonblocking(true)?;
            Ok(Some(listener))
        }
        Some(n) => Err(format!("systemd passed {n} sockets; axioma.socket must define one").into()),
    }
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

/// Prints the owner's one-time link. On a new installation it creates the
/// owner; afterwards it is the way back in when the password is lost.
async fn owner_link(config_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let config = Config::load(config_path)?;
    let data_dir = config.data_dir.clone();
    let (token, purpose) = tokio::task::spawn_blocking(move || {
        let (conn, _) = db::open(&data_dir)?;
        people::owner_link(&conn, true).map_err(Box::<dyn Error + Send + Sync>::from)
    })
    .await?
    .map_err(|err| err.to_string())?;
    let what = match purpose {
        people::Purpose::Setup => "create the owner of this installation",
        people::Purpose::Reset => "set a new password for the owner",
    };
    println!("Link to {what}, valid for 24 hours and usable once:");
    println!();
    println!("  {}/setup#{token}", config.panel_url());
    println!();
    println!("Any earlier link stops working.");
    Ok(())
}
