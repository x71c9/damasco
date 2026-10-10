//! axioma: self-hosted platform that builds and maintains a website through a
//! chat with a coding agent.

mod access;
mod activity;
mod agent;
mod bridge;
mod build;
mod cgroup;
mod chat;
mod check;
mod config;
mod db;
mod git;
mod hook;
mod http;
mod limit;
mod mcp;
mod panel;
mod people;
mod policy;
mod proxy;
mod runtime;
mod sandbox;
mod schema;
mod seccomp;
mod session;
mod site;
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
    /// Build the site once and update the preview.
    Build,
    /// Download the runtimes axioma manages (Node.js and Claude Code) when
    /// missing, and print their versions.
    Runtimes,
    /// Inside the agent sandbox: pass a hook event of the CLI to axioma and
    /// print its answer.
    #[command(hide = true)]
    Hook {
        #[arg(long, default_value = sandbox::HOOKS_IN_SANDBOX)]
        socket: PathBuf,
    },
    /// Inside the agent sandbox: axioma's MCP server for the CLI.
    #[command(hide = true)]
    Mcp {
        #[arg(long, default_value = sandbox::HOOKS_IN_SANDBOX)]
        socket: PathBuf,
    },
    /// Run a command in the sandbox, for development and diagnosis.
    #[command(hide = true)]
    SandboxRun {
        /// agent or build.
        #[arg(long, default_value = "build")]
        profile: String,
        /// Directory mounted read-write at /site.
        #[arg(long)]
        site: PathBuf,
        /// Directory mounted at /home/agent for the agent profile.
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Inside the sandbox: forward 127.0.0.1:3128 to the egress proxy and run
    /// the command.
    #[command(hide = true)]
    SandboxBridge {
        #[arg(long)]
        socket: PathBuf,
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let result = match args.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(&args.config).await,
        Command::Check => return check().await,
        Command::OwnerLink => owner_link(&args.config).await,
        Command::Runtimes => runtimes(&args.config).await,
        Command::Build => return build_once(&args.config).await,
        Command::SandboxRun {
            profile,
            site,
            home,
            command,
        } => return sandbox_run(&args.config, &profile, &site, home.as_deref(), &command).await,
        Command::Hook { socket } => return hook::run(&socket).await,
        Command::Mcp { socket } => return mcp::run(&socket).await,
        Command::SandboxBridge { socket, command } => {
            return bridge::run(&socket, &command).await;
        }
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

    prepare_site(&config)?;
    let limits = prepare_limits(&config)?;
    let builder = start_builder(&config, limits.as_ref()).await?;
    let account = start_agent(&config, limits.as_ref())?;
    let chat = chat::Chat::start(account.environment(), db.clone(), Some(builder.clone())).await;
    tokio::spawn(
        chat.clone()
            .serve_hooks(bind_hooks(&account.environment().hooks)?),
    );
    prepare_runtimes(&config, Some(builder.clone()))?;

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
    let closing = chat.clone();
    let app = http::router(config, db, builder, account, chat)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Open chat streams would keep the server waiting.
            closing.close();
            // The turn in progress saves what it wrote before the end.
            closing.wait_idle(std::time::Duration::from_secs(5)).await;
        })
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

/// Downloads missing managed runtimes and prints the version of each.
async fn runtimes(config_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let config = Config::load(config_path)?;
    let resolved = runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?;
    let resolved = if resolved.managed && !resolved.missing().is_empty() {
        let data_dir = config.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            runtime::install(&data_dir, &|step| eprintln!("axioma: {step}"))
        })
        .await??
    } else {
        resolved
    };
    print_versions(&resolved);
    Ok(())
}

fn print_versions(resolved: &runtime::Runtimes) {
    let origin = if resolved.managed {
        "managed"
    } else {
        "from the configuration"
    };
    for (name, version) in resolved.versions() {
        match version {
            Ok(version) => eprintln!("axioma: {name} {version} ({origin})"),
            Err(reason) => eprintln!("axioma: {name} cannot run: {reason}"),
        }
    }
}

/// At startup: reports the runtimes, and downloads missing managed ones in
/// the background so that the panel is available meanwhile.
fn prepare_runtimes(
    config: &Config,
    builder: Option<std::sync::Arc<build::Builder>>,
) -> Result<(), Box<dyn Error>> {
    let resolved = runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?;
    if !resolved.managed || resolved.missing().is_empty() {
        print_versions(&resolved);
        return Ok(());
    }
    let data_dir = config.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        match runtime::install(&data_dir, &|step| eprintln!("axioma: {step}")) {
            Ok(installed) => {
                print_versions(&installed);
                if let Some(builder) = builder {
                    builder.request();
                }
            }
            Err(err) => eprintln!("axioma: runtimes not installed: {err}"),
        }
    });
    Ok(())
}

/// Runs one command in the sandbox, with egress through a proxy that lives
/// for the duration of the command, and returns its exit code.
async fn sandbox_run(
    config_path: &std::path::Path,
    profile: &str,
    site: &std::path::Path,
    home: Option<&std::path::Path>,
    command: &[std::ffi::OsString],
) -> ExitCode {
    let run = async || -> Result<i32, Box<dyn Error>> {
        let config = Config::load(config_path)?;
        let runtimes = runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?;
        let profile = sandbox::Profile::parse(profile).ok_or("profile is agent or build")?;
        let bwrap = check::find_in_path(std::env::var_os("PATH").as_ref(), "bwrap")
            .ok_or("bwrap not found in PATH")?;
        let home = home
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| config.data_dir.join("agent"));
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(site)?;

        let run_dir = runtime_dir()?;
        let socket = run_dir.join(format!("sandbox-{}.sock", std::process::id()));
        let allow = allowlist(profile, &config);
        let server = {
            let socket = socket.clone();
            tokio::spawn(async move { proxy::serve(&socket, allow, "sandbox-run").await })
        };
        while !socket.exists() && !server.is_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        if server.is_finished() {
            return match server.await {
                Ok(Err(err)) => Err(format!("egress proxy on {}: {err}", socket.display()).into()),
                _ => Err("egress proxy stopped".into()),
            };
        }

        let limits = prepare_limits(&config)?;
        let procs = limits.as_ref().map(|groups| groups.procs(profile));
        let policy_dir = run_dir.join("policy");
        policy::write(&policy_dir)?;
        let axioma = std::env::current_exe()?;
        let sandbox = sandbox::Sandbox {
            profile,
            bwrap,
            site,
            agent_home: &home,
            runtimes: &runtimes,
            egress: Some(sandbox::Egress {
                axioma: &axioma,
                socket: &socket,
                hooks: None,
            }),
            policy: Some(&policy_dir),
            cgroup: procs.as_deref(),
        };
        let (program, args) = command.split_first().ok_or("no command")?;
        let program = program.to_str().ok_or("command is not UTF-8")?;
        let oom_before = procs.as_deref().map(cgroup::oom_kills);
        let (command, _filter) = sandbox.command(program, args)?.into_parts();
        let status = tokio::process::Command::from(command).status().await?;
        if let (Some(before), Some(procs)) = (oom_before, procs.as_deref())
            && cgroup::oom_kills(procs) > before
        {
            eprintln!("axioma: the sandbox was stopped: out of memory");
            server.abort();
            let _ = std::fs::remove_file(&socket);
            return Ok(137);
        }
        server.abort();
        let _ = std::fs::remove_file(&socket);
        Ok(status.code().unwrap_or(1))
    };
    match run().await {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("axioma: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Sets up the cgroups that limit the sandbox. Without delegation (outside
/// the systemd service) the sandbox runs without limits, and the log says so.
fn prepare_limits(config: &Config) -> Result<Option<cgroup::Cgroups>, Box<dyn Error>> {
    let total = cgroup::total_memory().ok_or("cannot read the machine's memory")?;
    let limits = cgroup::Limits::compute(&config.limits, total)?;
    match cgroup::Cgroups::prepare(&limits) {
        Ok(groups) => {
            eprintln!(
                "axioma: sandbox limits: agent {}, build {}, 512 processes each",
                cgroup::Size(limits.agent_memory),
                cgroup::Size(limits.build_memory)
            );
            Ok(Some(groups))
        }
        Err(reason) => {
            eprintln!("axioma: sandbox limits disabled: {reason}");
            Ok(None)
        }
    }
}

/// Short-lived files such as the proxy sockets. Unix socket paths are limited
/// to 108 bytes, so they live in the runtime directory systemd provides
/// (`RuntimeDirectory=axioma`), or in the user's one during development,
/// never under a data directory of arbitrary length.
fn runtime_dir() -> Result<std::path::PathBuf, Box<dyn Error>> {
    let dir = match (
        std::env::var_os("RUNTIME_DIRECTORY"),
        std::env::var_os("XDG_RUNTIME_DIR"),
    ) {
        (Some(dir), _) => std::path::PathBuf::from(dir),
        (None, Some(dir)) => std::path::PathBuf::from(dir).join("axioma"),
        (None, None) => std::env::temp_dir().join(format!("axioma-{}", unsafe { libc::getuid() })),
    };
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&dir)?;
    Ok(dir)
}

/// Creates the site repository on a new installation, named after the
/// site's first hostname until it is renamed in the panel.
fn prepare_site(config: &Config) -> Result<(), Box<dyn Error>> {
    let site = site::Site::at(&config.data_dir);
    if !site.exists() {
        let name = config.hosts.site.first().map_or("Site", String::as_str);
        site.create(name, "en")?;
        eprintln!(
            "axioma: created the site repository in {}",
            site.path.display()
        );
    }
    Ok(())
}

/// Built-in hosts of a profile plus the ones in `[sandbox]`.
fn allowlist(profile: sandbox::Profile, config: &Config) -> proxy::Allowlist {
    let (base, extra) = match profile {
        sandbox::Profile::Agent => (proxy::AGENT_ALLOW, &config.sandbox.agent_allow),
        sandbox::Profile::Build => (proxy::BUILD_ALLOW, &config.sandbox.build_allow),
    };
    let entries: Vec<&str> = base
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .collect();
    proxy::Allowlist::new(&entries)
}

/// Starts the egress proxy for builds and the build worker, and asks for a
/// first build when the preview has none yet.
async fn start_builder(
    config: &Config,
    limits: Option<&cgroup::Cgroups>,
) -> Result<std::sync::Arc<build::Builder>, Box<dyn Error>> {
    let environment = build_environment(config, limits)?;
    let socket = environment.proxy.clone();
    let allow = allowlist(sandbox::Profile::Build, config);
    tokio::spawn(async move {
        if let Err(err) = proxy::serve(&socket, allow, "build").await {
            eprintln!("axioma: build proxy stopped: {err}");
        }
    });
    let first = build::current_release(&environment.releases()).is_none();
    let builder = build::Builder::start(environment);
    if first {
        builder.request();
    }
    Ok(builder)
}

fn build_environment(
    config: &Config,
    limits: Option<&cgroup::Cgroups>,
) -> Result<build::Environment, Box<dyn Error>> {
    Ok(build::Environment {
        data_dir: config.data_dir.clone(),
        site: site::Site::at(&config.data_dir).path,
        runtimes: runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?,
        bwrap: check::find_in_path(std::env::var_os("PATH").as_ref(), "bwrap"),
        axioma: std::env::current_exe()?,
        proxy: runtime_dir()?.join("build.sock"),
        cgroup: limits.map(|groups| groups.procs(sandbox::Profile::Build)),
    })
}

/// Builds the site once, in the foreground, and prints the outcome.
async fn build_once(config_path: &std::path::Path) -> ExitCode {
    let run = async || -> Result<i32, Box<dyn Error>> {
        let config = Config::load(config_path)?;
        prepare_site(&config)?;
        let limits = prepare_limits(&config)?;
        let mut environment = build_environment(&config, limits.as_ref())?;
        environment.proxy = runtime_dir()?.join(format!("build-{}.sock", std::process::id()));
        let socket = environment.proxy.clone();
        let allow = allowlist(sandbox::Profile::Build, &config);
        let server = tokio::spawn(async move { proxy::serve(&socket, allow, "build").await });
        while !environment.proxy.exists() && !server.is_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let outcome = build::build(&environment).await;
        server.abort();
        let _ = std::fs::remove_file(&environment.proxy);
        match outcome {
            Ok((release, log)) => {
                eprint!("{log}");
                eprintln!("axioma: release {release} is in the preview");
                Ok(0)
            }
            Err((error, log)) => {
                eprint!("{log}");
                eprintln!("axioma: build failed: {error}");
                Ok(1)
            }
        }
    };
    match run().await {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("axioma: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Prepares the agent's sandbox (product rules, egress proxy, home) and the
/// management of its account.
fn start_agent(
    config: &Config,
    limits: Option<&cgroup::Cgroups>,
) -> Result<std::sync::Arc<agent::Account>, Box<dyn Error>> {
    let run_dir = runtime_dir()?;
    let policy_dir = run_dir.join("policy");
    policy::write(&policy_dir)?;
    let home = config.data_dir.join("agent");
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&home)?;
    let socket = run_dir.join("agent.sock");
    let allow = allowlist(sandbox::Profile::Agent, config);
    {
        let socket = socket.clone();
        tokio::spawn(async move {
            if let Err(err) = proxy::serve(&socket, allow, "agent").await {
                eprintln!("axioma: agent proxy stopped: {err}");
            }
        });
    }
    let environment = agent::Environment {
        site: site::Site::at(&config.data_dir).path,
        home,
        runtimes: runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?,
        bwrap: check::find_in_path(std::env::var_os("PATH").as_ref(), "bwrap"),
        axioma: std::env::current_exe()?,
        proxy: socket,
        hooks: run_dir.join("hooks.sock"),
        policy: policy_dir,
        cgroup: limits.map(|groups| groups.procs(sandbox::Profile::Agent)),
    };
    let account = std::sync::Arc::new(agent::Account::new(std::sync::Arc::new(environment)));
    agent::start_login_reaper(account.clone());
    Ok(account)
}

/// The socket where the chat answers the hooks of the CLI, readable only by
/// axioma's user. A socket left by an earlier run is replaced.
fn bind_hooks(path: &std::path::Path) -> Result<tokio::net::UnixListener, Box<dyn Error>> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("cannot replace {}: {err}", path.display()).into()),
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|err| format!("cannot listen on {}: {err}", path.display()))?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(listener)
}
