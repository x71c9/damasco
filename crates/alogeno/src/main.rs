//! alogeno: self-hosted platform that builds and maintains a website through a
//! chat with a coding agent.

mod activity;
mod agent;
mod assistant;
mod bridge;
mod build;
mod cgroup;
mod changes;
mod chat;
mod check;
mod cms;
mod components;
mod config;
mod db;
mod gate;
mod git;
mod hook;
mod http;
mod limit;
mod mcp;
mod media;
mod owner;
mod panel;
mod place;
mod policy;
mod preview;
mod proxy;
mod publish;
mod registry;
mod resolver;
mod runtime;
mod sandbox;
mod schema;
mod seccomp;
mod selection;
mod session;
mod site;
mod sites;
mod terminal;
mod token;
mod update;

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
        env = "ALOGENO_CONFIG",
        default_value = "/etc/alogeno/alogeno.toml"
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
    /// Download the runtimes alogeno manages (Node.js and Claude Code) when
    /// missing, and print their versions.
    Runtimes,
    /// As root: install the latest release (or --version) with its installer
    /// and restart every installation on the machine, keeping configuration
    /// and data.
    Update {
        /// Release to install, for example v0.1.2.
        #[arg(long)]
        version: Option<String>,
    },
    /// Inside the agent sandbox: pass a hook event of the CLI to alogeno and
    /// print its answer.
    #[command(hide = true)]
    Hook {
        #[arg(long, default_value = sandbox::HOOKS_IN_SANDBOX)]
        socket: PathBuf,
    },
    /// Inside the agent sandbox: alogeno's MCP server for the CLI.
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
        // Machine-wide: the binary is shared by every installation.
        Command::Update { version } => {
            tokio::task::spawn_blocking(move || update::run(version.as_deref()))
                .await
                .map_err(Box::<dyn Error>::from)
                .and_then(|result| result.map_err(Box::<dyn Error>::from))
        }
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
            eprintln!("alogeno: {err}");
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
        eprintln!("alogeno: environment ready for the sandbox");
    } else {
        for item in items.iter().filter(|i| i.level != Level::Ok) {
            eprintln!("alogeno: {} {}: {}", item.level, item.name, item.detail);
        }
        // Warnings leave the sandbox working (outside the service, without
        // its limits); a failed check stops it.
        if items.iter().any(|i| i.level == Level::Fail) {
            eprintln!(
                "alogeno: the assistant and the builds cannot run until `alogeno check` passes"
            );
        }
    }

    let data_dir = config.data_dir.clone();
    let (conn, opened) =
        tokio::task::spawn_blocking(move || db::open_installation(&data_dir)).await??;
    let db = db::Handle::new(conn);
    if let Some(backup) = &opened.backup {
        eprintln!(
            "alogeno: database copied to {} before upgrading",
            backup.display()
        );
    }
    eprintln!(
        "alogeno: database {} (schema v{})",
        opened.path.display(),
        opened.version
    );
    if let Some(legacy) = &opened.legacy {
        eprintln!(
            "alogeno: {} is a database of alogeno 1.x, not used: it can be removed",
            legacy.display()
        );
    }

    // A new installation prints the link that creates the owner, so that the
    // account is created in the panel by whoever can read this log. Every
    // start prints a new link until the owner exists; earlier ones stay valid.
    let has_owner = db.run(owner::has_owner).await?;
    if !has_owner {
        let (token, _) = db.run(|conn| owner::owner_link(conn, false)).await?;
        eprintln!();
        eprintln!("alogeno: no owner yet; create the owner's account with this one-time link:");
        eprintln!();
        eprintln!("    {}/setup#{token}", config.panel_url());
        eprintln!();
        eprintln!(
            "alogeno: it works once, for 24 hours; links printed by earlier starts still work"
        );
        eprintln!();
    }

    // Until sites are created from the panel, a new installation gets one.
    first_site(&db, &config).await?;
    let all = db.run(sites::list).await?;
    let resolver = resolver::Resolver::shared(&config, &all);
    let slot = gate::AgentSlot::new();
    let limits = prepare_limits(&config)?;
    let build_proxy = start_build_proxy(&config)?;
    let active = db.run(chat::assistant_in_use).await?;
    let (account, registry_slot) = start_agent(&config, limits.as_ref(), active, slot.clone())?;
    let factory: registry::Factory = {
        let config = config.clone();
        let build_cgroup = limits
            .as_ref()
            .map(|groups| groups.procs(sandbox::Profile::Build));
        let db = db.clone();
        let account = account.clone();
        let slot = slot.clone();
        std::sync::Arc::new(move |site: sites::Site| {
            let config = config.clone();
            let build_cgroup = build_cgroup.clone();
            let build_proxy = build_proxy.clone();
            let db = db.clone();
            let account = account.clone();
            let slot = slot.clone();
            Box::pin(async move {
                let root = sites::root(&config.data_dir, &site.id);
                let opening = root.clone();
                let (conn, _) = tokio::task::spawn_blocking(move || db::open_site(&opening))
                    .await
                    .map_err(|err| err.to_string())?
                    .map_err(|err| err.to_string())?;
                let site_db = db::Handle::new(conn);
                let environment = build_environment(&config, &root, build_proxy, build_cgroup)
                    .map_err(|err| err.to_string())?;
                let first = build::current_release(&environment.releases()).is_none();
                let builder = build::Builder::start(environment);
                if first && builder.builds_by_itself() {
                    builder.request();
                }
                let environment =
                    std::sync::Arc::new(account.environment().for_site(site::Site::at(&root).path));
                let chat = chat::Chat::start(
                    environment,
                    db,
                    site_db.clone(),
                    site.id.clone(),
                    slot,
                    Some(builder.clone()),
                )
                .await;
                eprintln!("alogeno: site {} at work", site.slug);
                Ok(std::sync::Arc::new(registry::SiteRuntime {
                    root,
                    db: site_db,
                    builder,
                    chat,
                }))
            })
        })
    };
    let registry = registry::Registry::new(db.clone(), config.clone(), resolver.clone(), factory);
    let _ = registry_slot.set(registry.clone());
    tokio::spawn(serve_hooks(
        bind_hooks(&account.environment().hooks)?,
        slot.clone(),
        registry.clone(),
    ));
    prepare_runtimes(&config, active, Some(registry.clone()))?;

    let listener = match activated_listener()? {
        Some(listener) => {
            eprintln!("alogeno: using the socket passed by systemd; `listen` is ignored");
            tokio::net::TcpListener::from_std(listener)?
        }
        None => {
            let addr = config.listen;
            tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|err| format!("cannot listen on {addr}: {err}"))?
        }
    };
    eprintln!("alogeno: listening on http://{}", listener.local_addr()?);
    eprintln!("alogeno: panel   {}", config.panel_url());
    for site in &all {
        eprintln!(
            "alogeno: site {}: preview {}",
            site.slug,
            config.url_of(&config.preview_host(&site.slug))
        );
        if let Some(host) = config.local_host(&site.slug) {
            eprintln!(
                "alogeno: site {}: published at {}",
                site.slug,
                config.url_of(&host)
            );
        }
    }
    let closing = registry.clone();
    let closing_slot = slot.clone();
    let app = http::router(config, db, registry, resolver, account, slot)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let chats: Vec<_> = closing
                .all_loaded()
                .into_iter()
                .map(|runtime| runtime.chat.clone())
                .collect();
            closing_slot.close();
            // Open chat streams would keep the server waiting.
            for chat in &chats {
                chat.close();
            }
            // The turn in progress saves what it wrote before the end.
            for chat in &chats {
                chat.wait_idle(std::time::Duration::from_secs(5)).await;
            }
        })
        .await?;
    eprintln!("alogeno: stopped");
    Ok(())
}

/// Answers the hooks of the CLI, and the requests of alogeno's MCP server
/// inside the sandbox, for the site whose assistant is at work; without
/// one, the connection closes unanswered.
async fn serve_hooks(
    listener: tokio::net::UnixListener,
    slot: std::sync::Arc<gate::AgentSlot>,
    registry: std::sync::Arc<registry::Registry>,
) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(err) => {
                eprintln!("alogeno: hook connection refused: {err}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let chat = slot
            .current()
            .and_then(|occupant| registry.loaded(&occupant.site_id))
            .map(|runtime| runtime.chat.clone());
        if let Some(chat) = chat {
            tokio::spawn(async move { chat.hook_connection(stream).await });
        }
    }
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
        Some(n) => {
            Err(format!("systemd passed {n} sockets; alogeno.socket must define one").into())
        }
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
        let (conn, _) = db::open_installation(&data_dir)?;
        owner::owner_link(&conn, true).map_err(Box::<dyn Error + Send + Sync>::from)
    })
    .await?
    .map_err(|err| err.to_string())?;
    let what = match purpose {
        owner::Purpose::Setup => "create the owner of this installation",
        owner::Purpose::Reset => "set a new password for the owner",
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
    let all = assistant::Provider::ALL;
    let missing = all.iter().any(|p| !resolved.missing(Some(*p)).is_empty());
    let resolved = if resolved.managed && missing {
        let data_dir = config.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            runtime::install(&data_dir, true, &all, &|step| eprintln!("alogeno: {step}"))
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
            Ok(version) => eprintln!("alogeno: {name} {version} ({origin})"),
            Err(reason) => eprintln!("alogeno: {name} cannot run: {reason}"),
        }
    }
}

/// At startup: reports the runtimes, and downloads missing managed ones in
/// the background so that the panel is available meanwhile.
fn prepare_runtimes(
    config: &Config,
    active: assistant::Provider,
    registry: Option<std::sync::Arc<registry::Registry>>,
) -> Result<(), Box<dyn Error>> {
    let resolved = runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?;
    let missing = resolved.missing(Some(active));
    if missing.is_empty() || !(resolved.managed || resolved.downloads(active)) {
        print_versions(&resolved);
        return Ok(());
    }
    let data_dir = config.data_dir.clone();
    let node = resolved.managed;
    tokio::task::spawn_blocking(move || {
        match runtime::install(&data_dir, node, &[active], &|step| {
            eprintln!("alogeno: {step}")
        }) {
            Ok(installed) => {
                print_versions(&installed);
                // The sites at work build with the runtimes just installed.
                for runtime in registry.iter().flat_map(|r| r.all_loaded()) {
                    if runtime.builder.builds_by_itself() {
                        runtime.builder.request();
                    }
                }
            }
            Err(err) => eprintln!("alogeno: runtimes not installed: {err}"),
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
        let home = home.map(std::path::Path::to_path_buf).unwrap_or_else(|| {
            config
                .data_dir
                .join("agent")
                .join(assistant::Provider::Claude.id())
        });
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(site)?;

        let run_dir = runtime_dir(&config.data_dir)?;
        let socket = run_dir.join(format!("sandbox-{}.sock", std::process::id()));
        // The agent profile runs Claude Code here: its hosts pass too.
        let mut entries = allowlist_entries(profile, &config);
        if profile == sandbox::Profile::Agent {
            entries.extend(
                assistant::Provider::Claude
                    .hosts()
                    .iter()
                    .map(|h| h.to_string()),
            );
        }
        let allow = proxy::Allowlist::new(&entries);
        let server = {
            let socket = socket.clone();
            tokio::spawn(async move { proxy::serve(&socket, allow, "sandbox-run", None).await })
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
        let alogeno = std::env::current_exe()?;
        let sandbox = sandbox::Sandbox {
            profile,
            bwrap,
            site,
            agent_home: &home,
            runtimes: &runtimes,
            egress: Some(sandbox::Egress {
                alogeno: &alogeno,
                socket: &socket,
                hooks: None,
            }),
            cli: Some(sandbox::Cli {
                provider: assistant::Provider::Claude,
                executable: &runtimes.claude,
                policy: &policy::managed(&policy_dir, assistant::Provider::Claude),
                locked: Vec::new(),
                locked_in_site: Vec::new(),
            }),
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
            eprintln!("alogeno: the sandbox was stopped: out of memory");
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
            eprintln!("alogeno: {err}");
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
                "alogeno: sandbox limits: agent {}, build {}, 512 processes each",
                cgroup::Size(limits.agent_memory),
                cgroup::Size(limits.build_memory)
            );
            Ok(Some(groups))
        }
        Err(reason) => {
            eprintln!("alogeno: sandbox limits disabled: {reason}");
            Ok(None)
        }
    }
}

/// Short-lived files such as the proxy sockets. Unix socket paths are limited
/// to 108 bytes, so they live in the runtime directory systemd provides
/// (`RuntimeDirectory=alogeno`), or in the user's one during development,
/// never under a data directory of arbitrary length. Outside systemd each
/// data directory gets its own folder, so two installations of the same
/// user (development and a test, say) never replace each other's sockets.
fn runtime_dir(data_dir: &std::path::Path) -> Result<std::path::PathBuf, Box<dyn Error>> {
    use sha2::Digest;
    let absolute = std::path::absolute(data_dir)?;
    let digest = sha2::Sha256::digest(absolute.as_os_str().as_encoded_bytes());
    let tag: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    let dir = match (
        std::env::var_os("RUNTIME_DIRECTORY"),
        std::env::var_os("XDG_RUNTIME_DIR"),
    ) {
        (Some(dir), _) => std::path::PathBuf::from(dir),
        (None, Some(dir)) => std::path::PathBuf::from(dir).join(format!("alogeno-{tag}")),
        (None, None) => {
            std::env::temp_dir().join(format!("alogeno-{}-{tag}", unsafe { libc::getuid() }))
        }
    };
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&dir)?;
    Ok(dir)
}

/// The site the installation serves: until sites are created from the
/// panel, the first one, created on a new installation and named after the
/// first hostname of the site. Folders left by a creation cut short go
/// first.
async fn first_site(db: &db::Handle, config: &Config) -> Result<sites::Site, Box<dyn Error>> {
    let data_dir = config.data_dir.clone();
    Ok(db
        .run(move |conn| ensure_first_site(conn, &data_dir, "Site"))
        .await?)
}

fn ensure_first_site(
    conn: &rusqlite::Connection,
    data_dir: &std::path::Path,
    name: &str,
) -> Result<sites::Site, String> {
    for removed in sites::sweep(conn, data_dir)? {
        eprintln!(
            "alogeno: removed {}, left by a site never created",
            removed.display()
        );
    }
    if let Some(site) = sites::list(conn)
        .map_err(|err| err.to_string())?
        .into_iter()
        .next()
    {
        return Ok(site);
    }
    let site = sites::create(conn, data_dir, name, "main")?;
    eprintln!(
        "alogeno: created the site {} in {}",
        site.slug,
        sites::root(data_dir, &site.id).display()
    );
    Ok(site)
}

/// Built-in hosts of a profile plus the ones in `[sandbox]`.
fn allowlist(profile: sandbox::Profile, config: &Config) -> proxy::Allowlist {
    proxy::Allowlist::new(&allowlist_entries(profile, config))
}

fn allowlist_entries(profile: sandbox::Profile, config: &Config) -> Vec<String> {
    let (base, extra) = match profile {
        sandbox::Profile::Agent => (proxy::AGENT_ALLOW, &config.sandbox.agent_allow),
        sandbox::Profile::Build => (proxy::BUILD_ALLOW, &config.sandbox.build_allow),
    };
    base.iter()
        .map(|h| h.to_string())
        .chain(extra.iter().cloned())
        .collect()
}

/// Starts the egress proxy that every site's builds share, and returns its
/// socket.
fn start_build_proxy(config: &Config) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let socket = runtime_dir(&config.data_dir)?.join("build.sock");
    let serving = socket.clone();
    let allow = allowlist(sandbox::Profile::Build, config);
    tokio::spawn(async move {
        if let Err(err) = proxy::serve(&serving, allow, "build", None).await {
            eprintln!("alogeno: build proxy stopped: {err}");
        }
    });
    Ok(socket)
}

/// The builds of the site in `site_root`, which is laid out as the data
/// directory of alogeno 1.x was: the repository, the releases, the build
/// folders and the pictures are under it. Runtimes, the egress proxy and
/// the cgroup belong to the installation.
fn build_environment(
    config: &Config,
    site_root: &std::path::Path,
    proxy: std::path::PathBuf,
    cgroup: Option<std::path::PathBuf>,
) -> Result<build::Environment, Box<dyn Error>> {
    Ok(build::Environment {
        data_dir: site_root.to_path_buf(),
        site: site::Site::at(site_root).path,
        runtimes: runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?,
        bwrap: check::find_in_path(std::env::var_os("PATH").as_ref(), "bwrap"),
        alogeno: std::env::current_exe()?,
        proxy,
        cgroup,
    })
}

/// Builds the first site once, in the foreground, and prints the outcome.
async fn build_once(config_path: &std::path::Path) -> ExitCode {
    let run = async || -> Result<i32, Box<dyn Error>> {
        let config = Config::load(config_path)?;
        let (conn, _) = db::open_installation(&config.data_dir)?;
        let site = ensure_first_site(&conn, &config.data_dir, "Site")?;
        let site_root = sites::root(&config.data_dir, &site.id);
        let limits = prepare_limits(&config)?;
        let socket =
            runtime_dir(&config.data_dir)?.join(format!("build-{}.sock", std::process::id()));
        let environment = build_environment(
            &config,
            &site_root,
            socket.clone(),
            limits
                .as_ref()
                .map(|groups| groups.procs(sandbox::Profile::Build)),
        )?;
        let allow = allowlist(sandbox::Profile::Build, &config);
        let serving = socket.clone();
        let server =
            tokio::spawn(async move { proxy::serve(&serving, allow, "build", None).await });
        while !socket.exists() && !server.is_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let outcome = build::build(&environment).await;
        server.abort();
        let _ = std::fs::remove_file(&socket);
        match outcome {
            Ok((release, log)) => {
                eprint!("{log}");
                eprintln!("alogeno: release {release} is in the preview");
                Ok(0)
            }
            Err((error, log)) => {
                eprint!("{log}");
                eprintln!("alogeno: build failed: {error}");
                Ok(1)
            }
        }
    };
    match run().await {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("alogeno: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The sites, once started: the egress proxies ask them about hosts outside
/// the allowlist.
type RegistrySlot = std::sync::Arc<std::sync::OnceLock<std::sync::Arc<registry::Registry>>>;

/// Prepares the agent's sandbox (product rules, egress proxy, home) and the
/// management of its account. The account's own requests (sign-in, status,
/// models) see an empty site; each site's chat gets an environment with its
/// own folder (`Environment::for_site`). A host outside the allowlist is
/// asked of the site whose assistant is at work.
fn start_agent(
    config: &Config,
    limits: Option<&cgroup::Cgroups>,
    active: assistant::Provider,
    slot: std::sync::Arc<gate::AgentSlot>,
) -> Result<(std::sync::Arc<agent::Account>, RegistrySlot), Box<dyn Error>> {
    let run_dir = runtime_dir(&config.data_dir)?;
    let policy_dir = run_dir.join("policy");
    policy::write(&policy_dir)?;
    let homes = config.data_dir.join("agent");
    agent::separate_homes(&homes)?;
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&homes)?;
    let no_site = run_dir.join("no-site");
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(&no_site)?;
    let registry: RegistrySlot = Default::default();
    let environment = std::sync::Arc::new(agent::Environment::new(
        no_site,
        homes,
        runtime::Runtimes::resolve(&config.runtimes, &config.data_dir)?,
        check::find_in_path(std::env::var_os("PATH").as_ref(), "bwrap"),
        std::env::current_exe()?,
        run_dir.clone(),
        run_dir.join("hooks.sock"),
        policy_dir,
        limits.map(|groups| groups.procs(sandbox::Profile::Agent)),
        active,
    ));
    // One proxy per assistant: its CLI reaches its own hosts, whether it is
    // the one in use or signing in; any other host waits for a person.
    let approve: proxy::Approve = {
        let registry = registry.clone();
        std::sync::Arc::new(move |host| {
            let chat = slot
                .current()
                .and_then(|occupant| registry.get()?.loaded(&occupant.site_id))
                .map(|runtime| runtime.chat.clone());
            Box::pin(async move {
                match chat {
                    Some(chat) => chat.approve_host(host).await,
                    None => false,
                }
            })
        })
    };
    for provider in assistant::Provider::ALL {
        let socket = environment.proxy(provider);
        let mut entries: Vec<String> = allowlist_entries(sandbox::Profile::Agent, config);
        entries.extend(provider.hosts().iter().map(|h| h.to_string()));
        let allow = proxy::Allowlist::new(&entries);
        let approve = approve.clone();
        let name = match provider {
            assistant::Provider::Claude => "agent-claude",
            assistant::Provider::Codex => "agent-codex",
        };
        tokio::spawn(async move {
            if let Err(err) = proxy::serve(&socket, allow, name, Some(approve)).await {
                eprintln!("alogeno: {name} proxy stopped: {err}");
            }
        });
    }
    let account = std::sync::Arc::new(agent::Account::new(environment, config.data_dir.clone()));
    agent::start_login_reaper(account.clone());
    Ok((account, registry))
}

/// The socket where the chat answers the hooks of the CLI, readable only by
/// alogeno's user. A socket left by an earlier run is replaced.
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
