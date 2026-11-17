//! The coding agent: the official CLI of an assistant (Claude Code or
//! Codex), run in the agent sandbox with the account of the site's owner.
//! One assistant is in use at a time; each has its own home, so the active
//! one never reads the credentials of another.
//!
//! This module covers the accounts: whether a CLI is signed in, and the
//! sign-in itself.
//!
//! - Claude: `claude auth login` prints the address of Anthropic's sign-in
//!   page and waits for the code that page shows at the end; the panel opens
//!   the address and sends the code back. An Anthropic API key is the other
//!   way: axioma checks it with Anthropic, keeps it in a file of the agent's
//!   home and has the CLI read it through `apiKeyHelper`, so it never
//!   appears in a command line or in the environment of other processes.
//! - Codex: `codex app-server` reports the account and signs in with a
//!   device code: the panel shows the code and the address where the person
//!   enters it, and axioma waits for Codex to confirm. An OpenAI API key is
//!   checked with OpenAI and given to Codex on its input.
//!
//! The credentials stay in the assistant's home, inside the sandbox. One way
//! at a time: a key replaces the subscription's sign-in, and disconnecting
//! removes both.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;

use crate::assistant::{Capabilities, Model, Provider, codex};
use crate::runtime::Runtimes;
use crate::sandbox::{Cli, Egress, Profile, Sandbox};

/// Everything the agent's sandbox needs.
pub struct Environment {
    pub site: PathBuf,
    /// The homes of the assistants, one directory each: sessions, memory,
    /// credentials.
    pub homes: PathBuf,
    pub runtimes: Runtimes,
    pub bwrap: Option<PathBuf>,
    pub axioma: PathBuf,
    /// Directory with the sockets of the egress proxies, one per assistant.
    pub proxies: PathBuf,
    /// Socket where the chat answers the hooks of the CLI.
    pub hooks: PathBuf,
    /// Directory with the product rules of every assistant.
    pub policy: PathBuf,
    pub cgroup: Option<PathBuf>,
    /// The assistant in use.
    active: std::sync::RwLock<Provider>,
    /// In tests, a program that stands in for every CLI, run without the
    /// sandbox.
    #[cfg(test)]
    pub fake: std::sync::Mutex<Option<PathBuf>>,
}

impl Environment {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        site: PathBuf,
        homes: PathBuf,
        runtimes: Runtimes,
        bwrap: Option<PathBuf>,
        axioma: PathBuf,
        proxies: PathBuf,
        hooks: PathBuf,
        policy: PathBuf,
        cgroup: Option<PathBuf>,
        active: Provider,
    ) -> Self {
        Self {
            site,
            homes,
            runtimes,
            bwrap,
            axioma,
            proxies,
            hooks,
            policy,
            cgroup,
            active: std::sync::RwLock::new(active),
            #[cfg(test)]
            fake: std::sync::Mutex::new(None),
        }
    }

    /// The egress proxy of `provider`: the agent allowlist with its hosts.
    pub fn proxy(&self, provider: Provider) -> PathBuf {
        self.proxies.join(format!("agent-{}.sock", provider.id()))
    }

    pub fn active(&self) -> Provider {
        *self.active.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_active(&self, provider: Provider) {
        *self.active.write().unwrap_or_else(|e| e.into_inner()) = provider;
    }

    /// The home of `provider`'s CLI, mounted at /home/agent when it runs.
    pub fn home(&self, provider: Provider) -> PathBuf {
        self.homes.join(provider.id())
    }

    /// Whether `provider` keeps credentials in its home. Only the presence
    /// of the files is looked at.
    pub fn saved(&self, provider: Provider) -> bool {
        let home = self.home(provider);
        match provider {
            Provider::Claude => {
                home.join(".claude/.credentials.json").is_file() || home.join(KEY_FILE).is_file()
            }
            Provider::Codex => home.join(".codex/auth.json").is_file(),
        }
    }

    fn prepared(
        &self,
        provider: Provider,
        args: &[&str],
        terminal: bool,
    ) -> Result<crate::sandbox::Prepared, String> {
        let bwrap = self.bwrap.clone().ok_or("bwrap is not installed")?;
        if !self.runtimes.missing(Some(provider)).is_empty() {
            return Err("the assistant is still being downloaded".into());
        }
        let executable = self
            .runtimes
            .cli(provider)
            .ok_or("this assistant is not available on this installation")?;
        let home = self.home(provider);
        std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
            .create(&home)
            .map_err(|e| format!("{}: {e}", home.display()))?;
        let policy = crate::policy::managed(&self.policy, provider);
        let proxy = self.proxy(provider);
        let sandbox = Sandbox {
            profile: Profile::Agent,
            bwrap,
            site: &self.site,
            agent_home: &home,
            runtimes: &self.runtimes,
            egress: Some(Egress {
                axioma: &self.axioma,
                socket: &proxy,
                hooks: Some(self.hooks.as_path()).filter(|path| path.exists()),
            }),
            cli: Some(Cli {
                provider,
                executable,
                policy: &policy,
                locked: crate::policy::locked(&self.policy, provider),
                locked_in_site: crate::policy::locked_in_site(&self.policy, provider),
            }),
            cgroup: self.cgroup.as_deref(),
        };
        let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
        let program = provider.program();
        if terminal {
            sandbox.terminal_command(program, &args)
        } else {
            sandbox.command(program, &args)
        }
        .map_err(|e| e.to_string())
    }

    /// A command of `provider`'s CLI in the sandbox, with piped input and
    /// output. The returned descriptor holds the system call filter: keep
    /// it open until the command is spawned.
    pub fn cli(
        &self,
        provider: Provider,
        args: &[&str],
    ) -> Result<(tokio::process::Command, std::os::fd::OwnedFd), String> {
        #[cfg(test)]
        if let Some(fake) = self.fake.lock().unwrap().clone() {
            let mut command = tokio::process::Command::new(fake);
            command
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let filter = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
            return Ok((command, filter.into()));
        }
        let (command, filter) = self.prepared(provider, args, false)?.into_parts();
        let mut command = tokio::process::Command::from(command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok((command, filter))
    }

    /// A command of Claude Code in the sandbox.
    pub fn claude(
        &self,
        args: &[&str],
    ) -> Result<(tokio::process::Command, std::os::fd::OwnedFd), String> {
        self.cli(Provider::Claude, args)
    }

    /// `codex app-server` in the sandbox, connected and initialized.
    pub async fn codex_server(&self) -> Result<codex::Server, String> {
        let (command, filter) = self.cli(Provider::Codex, &["app-server"])?;
        let server = codex::Server::start(command).await;
        drop(filter);
        server
    }

    /// Prepares the CLI's settings in the agent's home for the interactive
    /// mode: the first-run screens are done, `/site` is trusted (axioma
    /// chose it) and the text style follows the panel's `theme`. Other
    /// settings stay as the CLI wrote them.
    pub fn prepare_terminal(&self, theme: &str) -> Result<(), String> {
        let home = self.home(Provider::Claude);
        let path = home.join(".claude.json");
        let mut settings: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        settings["hasCompletedOnboarding"] = true.into();
        settings["theme"] = if theme == "light" { "light" } else { "dark" }.into();
        if !settings["projects"].is_object() {
            settings["projects"] = serde_json::json!({});
        }
        if !settings["projects"]["/site"].is_object() {
            settings["projects"]["/site"] = serde_json::json!({});
        }
        settings["projects"]["/site"]["hasTrustDialogAccepted"] = true.into();
        settings["projects"]["/site"]["hasCompletedProjectOnboarding"] = true.into();
        let temporary = home.join(".claude.json.axioma");
        let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
        std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
        std::fs::set_permissions(
            &temporary,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &path).map_err(|e| e.to_string())
    }

    /// The interactive CLI of `provider` in the sandbox, for a terminal: the
    /// caller attaches it. The descriptor holds the system call filter until
    /// the command is spawned.
    pub fn cli_terminal(
        &self,
        provider: Provider,
        args: &[&str],
    ) -> Result<(std::process::Command, std::os::fd::OwnedFd), String> {
        Ok(self.prepared(provider, args, true)?.into_parts())
    }
}

/// Moves the home of a single assistant (before Codex) into the home of
/// Claude, once: `homes` then holds one directory per assistant.
pub fn separate_homes(homes: &Path) -> std::io::Result<()> {
    let claude = homes.join(Provider::Claude.id());
    let single = homes.join(".claude").exists() || homes.join(".claude.json").exists();
    if claude.exists() || !single {
        return Ok(());
    }
    let parent = homes.parent().unwrap_or(homes);
    let moving = parent.join(".agent-moving");
    std::fs::rename(homes, &moving)?;
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(homes)?;
    std::fs::rename(&moving, &claude)
}

/// What the panel shows about the agent's account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountStatus {
    pub provider: Provider,
    pub signed_in: bool,
    /// How the CLI is signed in: "claude.ai" or "api_key_helper" for
    /// Claude, "chatgpt" or "apiKey" for Codex, as the CLI reports it.
    pub method: Option<String>,
    /// The last characters of the API key, to tell which key it is.
    pub key_ending: Option<String>,
    /// When the sign-in expires, as the CLI reports it.
    pub expires_at: Option<String>,
    /// The account's address and plan, when the CLI reports them.
    pub email: Option<String>,
    pub plan: Option<String>,
    /// A sign-in is waiting for its code.
    pub signing_in: bool,
    /// For a sign-in with a device code: the code to enter and where.
    pub user_code: Option<String>,
    pub verification_url: Option<String>,
    /// Why the account cannot be checked now, if so.
    pub problem: Option<String>,
}

impl AccountStatus {
    fn of(provider: Provider) -> Self {
        Self {
            provider,
            signed_in: false,
            method: None,
            key_ending: None,
            expires_at: None,
            email: None,
            plan: None,
            signing_in: false,
            user_code: None,
            verification_url: None,
            problem: None,
        }
    }
}

/// An assistant this installation offers.
#[derive(Debug, Clone, Serialize)]
pub struct Offered {
    pub provider: Provider,
    pub name: &'static str,
    /// It keeps a sign-in in its home.
    pub saved: bool,
    pub capabilities: Capabilities,
    /// The modes of its CLI the chat offers; the first is the default.
    pub modes: &'static [&'static str],
    /// Hosts it reaches on its own, without asking.
    pub hosts: &'static [&'static str],
}

/// How a sign-in starts: the address to open and, for a device code, the
/// code to enter there.
#[derive(Debug, Clone, Serialize)]
pub struct LoginStart {
    pub url: String,
    pub code: Option<String>,
    /// The assistant is signed in already: nothing to do.
    pub connected: bool,
}

/// What the CLI did with a code.
enum Answer {
    Refused,
    Ended,
}

struct Login {
    child: Child,
    stdin: ChildStdin,
    /// Lines the CLI prints after the address, to read its answer to a code.
    lines: tokio::sync::mpsc::UnboundedReceiver<String>,
    started: std::time::Instant,
}

/// A sign-in of Codex with a device code, waiting for the person.
struct DeviceLogin {
    url: String,
    code: String,
    started: std::time::Instant,
    /// The outcome once Codex reports it; the task holds the server.
    outcome: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    task: tokio::task::JoinHandle<()>,
}

pub struct Account {
    env: Arc<Environment>,
    login: Mutex<Option<Login>>,
    device: Mutex<Option<DeviceLogin>>,
    /// One request of Codex's account at a time: its credentials file is
    /// shared.
    codex: Mutex<()>,
    /// A download of a CLI in progress.
    downloading: std::sync::Mutex<Option<Provider>>,
    /// Why the last sign-in of Claude ended before its code, if it did.
    login_ended: std::sync::Mutex<Option<String>>,
    /// The models each CLI offers the account, once asked.
    claude_models: Mutex<Option<Vec<Model>>>,
    codex_models: Mutex<Option<Vec<Model>>>,
    data_dir: PathBuf,
}

/// How long a sign-in waits for its code.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

impl Account {
    pub fn new(env: Arc<Environment>, data_dir: PathBuf) -> Self {
        Self {
            env,
            login: Mutex::new(None),
            device: Mutex::new(None),
            codex: Mutex::new(()),
            downloading: std::sync::Mutex::new(None),
            login_ended: std::sync::Mutex::new(None),
            claude_models: Mutex::new(None),
            codex_models: Mutex::new(None),
            data_dir,
        }
    }

    pub fn environment(&self) -> Arc<Environment> {
        self.env.clone()
    }

    /// The assistants this installation offers.
    pub fn offered(&self) -> Vec<Offered> {
        Provider::ALL
            .into_iter()
            .filter(|p| self.env.runtimes.cli(*p).is_some())
            .map(|provider| Offered {
                provider,
                name: provider.name(),
                saved: self.env.saved(provider),
                capabilities: provider.capabilities(),
                modes: provider.modes(),
                hosts: provider.hosts(),
            })
            .collect()
    }

    pub async fn status_of(&self, provider: Provider) -> AccountStatus {
        if let Some(problem) = self.download(provider) {
            let mut status = AccountStatus::of(provider);
            status.problem = Some(problem);
            return status;
        }
        match provider {
            Provider::Claude => {
                let signing_in = self.login.lock().await.is_some();
                self.claude_status(signing_in).await
            }
            Provider::Codex => self.codex_status().await,
        }
    }

    /// Starts downloading `provider`'s CLI when it is missing; the reason
    /// it cannot run yet, if so.
    fn download(&self, provider: Provider) -> Option<String> {
        let runtimes = &self.env.runtimes;
        if runtimes.missing(Some(provider)).is_empty() {
            return None;
        }
        if !runtimes.downloads(provider) {
            return Some(format!(
                "{} is not installed: set [runtimes] in the configuration",
                provider.name()
            ));
        }
        let mut downloading = self.downloading.lock().unwrap_or_else(|e| e.into_inner());
        if downloading.is_none() {
            *downloading = Some(provider);
            let data_dir = self.data_dir.clone();
            let node = runtimes.managed;
            tokio::task::spawn_blocking(move || {
                let outcome = crate::runtime::install(&data_dir, node, &[provider], &|step| {
                    eprintln!("axioma: {step}")
                });
                if let Err(err) = outcome {
                    eprintln!("axioma: {} not installed: {err}", provider.name());
                }
            });
        }
        Some("the assistant is still being downloaded".into())
    }

    /// Clears the download flag once the CLI is on disk.
    fn downloaded(&self) {
        let mut downloading = self.downloading.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(provider) = *downloading
            && self.env.runtimes.missing(Some(provider)).is_empty()
        {
            *downloading = None;
        }
    }

    async fn forget_models(&self, provider: Provider) {
        match provider {
            Provider::Claude => *self.claude_models.lock().await = None,
            Provider::Codex => *self.codex_models.lock().await = None,
        }
    }

    /// Starts a sign-in of `provider` with its subscription.
    pub async fn start_login(&self, provider: Provider) -> Result<LoginStart, String> {
        self.forget_models(provider).await;
        self.downloaded();
        if let Some(problem) = self.download(provider) {
            return Err(problem);
        }
        // Still signed in from before: a new sign-in would end at once.
        if self.status_of(provider).await.signed_in {
            return Ok(LoginStart {
                url: String::new(),
                code: None,
                connected: true,
            });
        }
        match provider {
            Provider::Claude => self.claude_login().await.map(|url| LoginStart {
                url,
                code: None,
                connected: false,
            }),
            Provider::Codex => self.codex_login().await,
        }
    }

    /// Connects `provider` with an API key, once its vendor accepts it. The
    /// subscription's sign-in, if any, is removed.
    pub async fn use_key(&self, provider: Provider, key: &str) -> Result<(), String> {
        // Another account may see other models.
        self.forget_models(provider).await;
        if let Some(problem) = self.download(provider) {
            return Err(problem);
        }
        match provider {
            Provider::Claude => self.claude_key(key).await,
            Provider::Codex => self.codex_key(key).await,
        }
    }

    pub async fn logout(&self, provider: Provider) -> Result<(), String> {
        match provider {
            Provider::Claude => self.claude_logout().await,
            Provider::Codex => self.codex_logout().await,
        }
    }

    /// The models `provider`'s CLI can use. Codex's depend on the account:
    /// it is asked once.
    pub async fn models(&self, provider: Provider) -> Result<Vec<Model>, String> {
        match provider {
            Provider::Claude => {
                let mut cached = self.claude_models.lock().await;
                if let Some(models) = cached.as_ref() {
                    return Ok(models.clone());
                }
                let models = self.claude_models_now().await?;
                if !models.is_empty() {
                    *cached = Some(models.clone());
                }
                Ok(models)
            }
            Provider::Codex => {
                let mut cached = self.codex_models.lock().await;
                if let Some(models) = cached.as_ref() {
                    return Ok(models.clone());
                }
                let _one = self.codex.lock().await;
                let mut server = self.env.codex_server().await?;
                let listed = server.call("model/list", json!({ "limit": 100 })).await;
                server.stop().await;
                let models = codex::models_of(&listed?);
                if !models.is_empty() {
                    *cached = Some(models.clone());
                }
                Ok(models)
            }
        }
    }

    /// Stops sign-ins that waited too long.
    pub async fn expire_login(&self) {
        self.downloaded();
        let mut slot = self.login.lock().await;
        if let Some(login) = slot.as_mut() {
            let ended = login.child.try_wait().ok().flatten();
            if ended.is_some() || login.started.elapsed() > LOGIN_TIMEOUT {
                // What the CLI said last tells why it ended.
                let mut said = Vec::new();
                while let Ok(line) = login.lines.try_recv() {
                    if !line.trim().is_empty() {
                        said.push(line.trim().to_string());
                    }
                }
                let last = said.last().cloned().unwrap_or_default();
                let reason = match ended {
                    Some(status) => {
                        eprintln!(
                            "axioma: Claude's sign-in ended before its code ({status}): {}",
                            said.join(" / ")
                        );
                        format!("the sign-in of Claude ended by itself ({last}); start it again")
                    }
                    None => "the sign-in waited too long; start it again".to_string(),
                };
                *self.login_ended.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
                *slot = None;
            }
        }
        drop(slot);
        let mut device = self.device.lock().await;
        if device
            .as_ref()
            .is_some_and(|login| login.started.elapsed() > LOGIN_TIMEOUT)
            && let Some(login) = device.take()
        {
            login.task.abort();
        }
    }
}

// Claude Code.
impl Account {
    /// Asks Claude Code for its models and stops it: no message is sent.
    async fn claude_models_now(&self) -> Result<Vec<Model>, String> {
        let (mut command, filter) = self.env.claude(crate::assistant::claude::MODELS_ARGS)?;
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        drop(filter);
        let mut stdin = child.stdin.take().ok_or("no input to Claude")?;
        let stdout = child.stdout.take().ok_or("no output from Claude")?;
        let request = crate::assistant::claude::models_request();
        stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut lines = BufReader::new(stdout).lines();
        let models = tokio::time::timeout(Duration::from_secs(30), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(models) = crate::assistant::claude::models_of(&line) {
                    return Some(models);
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        let _ = child.start_kill();
        let _ = child.wait().await;
        models.ok_or_else(|| "Claude did not give its models".to_string())
    }

    async fn claude_status(&self, signing_in: bool) -> AccountStatus {
        let home = self.env.home(Provider::Claude);
        let output = match self.env.claude(&["auth", "status", "--json"]) {
            Ok((mut command, _filter)) => command.output().await.map_err(|e| e.to_string()),
            Err(problem) => Err(problem),
        };
        let parsed = output.and_then(|output| {
            serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .map_err(|_| String::from_utf8_lossy(&output.stderr).trim().to_string())
        });
        let mut status = AccountStatus::of(Provider::Claude);
        status.signing_in = signing_in;
        match parsed {
            Ok(json) => {
                status.signed_in = json["loggedIn"].as_bool().unwrap_or(false);
                status.method = json["authMethod"]
                    .as_str()
                    .filter(|m| *m != "none")
                    .map(String::from);
                status.key_ending = (json["authMethod"] == "api_key_helper")
                    .then(|| key_ending(&home))
                    .flatten();
                status.expires_at = json["loginExpiresAt"].as_str().map(String::from);
                status.email = json["email"].as_str().map(String::from);
            }
            Err(problem) => status.problem = Some(problem),
        }
        status
    }

    /// Starts `claude auth login` and returns the address of the sign-in
    /// page. A sign-in already waiting is replaced.
    async fn claude_login(&self) -> Result<String, String> {
        let mut slot = self.login.lock().await;
        *slot = None;
        *self.login_ended.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let (mut command, _filter) = self.env.claude(&["auth", "login", "--claudeai"])?;
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("no input to the sign-in")?;
        let stdout = child.stdout.take().ok_or("no output from the sign-in")?;
        let mut lines = BufReader::new(stdout).lines();
        let url = tokio::time::timeout(Duration::from_secs(30), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(start) = line.find("https://") {
                    return Some(line[start..].trim().to_string());
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
        .ok_or("the sign-in did not provide an address")?;
        // Keep reading the output, so that the CLI never blocks on a full pipe
        // and its answer to a code can be read.
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let errors = sender.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        // The CLI reports a refused code on its error output.
        if let Some(stderr) = child.stderr.take() {
            let mut lines = BufReader::new(stderr).lines();
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    if errors.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        *slot = Some(Login {
            child,
            stdin,
            lines: receiver,
            started: std::time::Instant::now(),
        });
        Ok(url)
    }

    /// Sends the code shown by Claude's sign-in page. A code the CLI
    /// refuses leaves the sign-in open, so that another code can be sent.
    pub async fn finish_login(&self, code: &str) -> Result<(), String> {
        let mut slot = self.login.lock().await;
        let Some(mut login) = slot.take() else {
            let ended = self
                .login_ended
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            return Err(
                ended.unwrap_or_else(|| "no sign-in is waiting for a code; start again".into())
            );
        };
        let code = code.trim();
        if code.is_empty() || code.len() > 512 || code.contains(char::is_whitespace) {
            *slot = Some(login);
            return Err("the code is not valid".into());
        }
        login
            .stdin
            .write_all(format!("{code}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        // After a valid code the CLI saves the credentials but may not exit
        // at once: the account's status is checked every few seconds too.
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.tick().await;
        let outcome = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                tokio::select! {
                    status = login.child.wait() => {
                        let _ = status;
                        return Answer::Ended;
                    }
                    line = login.lines.recv() => match line {
                        Some(line) if line.to_ascii_lowercase().contains("invalid") => return Answer::Refused,
                        Some(line) if line.to_ascii_lowercase().contains("success") => return Answer::Ended,
                        Some(_) => {}
                        None => return Answer::Ended,
                    },
                    _ = tick.tick() => {
                        if self.claude_status(false).await.signed_in {
                            return Answer::Ended;
                        }
                    }
                }
            }
        })
        .await;
        match outcome {
            Ok(Answer::Refused) => {
                *slot = Some(login);
                Err("code not accepted: copy all of it".into())
            }
            Ok(Answer::Ended) | Err(_) => {
                // The process is no longer needed, whatever it was doing.
                let _ = login.child.start_kill();
                if self.claude_status(false).await.signed_in {
                    Ok(())
                } else if outcome.is_err() {
                    Err("the sign-in did not answer; start it again".into())
                } else {
                    Err("the sign-in failed; start it again".into())
                }
            }
        }
    }

    async fn claude_key(&self, key: &str) -> Result<(), String> {
        let home = self.env.home(Provider::Claude);
        let key = valid_key(key)?.to_string();
        let checked = key.clone();
        tokio::task::spawn_blocking(move || check_key(&checked))
            .await
            .map_err(|e| e.to_string())??;
        // Not signed in with a subscription is fine too.
        let _ = self.claude_logout().await;
        keep_key(&home, &key)?;
        if self.claude_status(false).await.signed_in {
            Ok(())
        } else {
            forget_key(&home)?;
            Err("the assistant could not use the key".into())
        }
    }

    async fn claude_logout(&self) -> Result<(), String> {
        *self.claude_models.lock().await = None;
        *self.login.lock().await = None;
        forget_key(&self.env.home(Provider::Claude))?;
        let (mut command, _filter) = self.env.claude(&["auth", "logout"])?;
        let output = command.output().await.map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}

// Codex.
impl Account {
    async fn codex_status(&self) -> AccountStatus {
        let mut status = AccountStatus::of(Provider::Codex);
        {
            let mut device = self.device.lock().await;
            if let Some(login) = device.as_ref() {
                let outcome = login
                    .outcome
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                match outcome {
                    None => {
                        status.signing_in = true;
                        status.user_code = Some(login.code.clone());
                        status.verification_url = Some(login.url.clone());
                    }
                    Some(Err(problem)) => {
                        status.problem = Some(problem);
                        *device = None;
                    }
                    Some(Ok(())) => *device = None,
                }
            }
        }
        let _one = self.codex.lock().await;
        let read = async {
            let mut server = self.env.codex_server().await?;
            let result = server
                .call("account/read", json!({ "refreshToken": false }))
                .await;
            server.stop().await;
            result
        };
        match read.await {
            Ok(result) => {
                let account = codex::account_of(&result);
                status.signed_in = account.signed_in;
                status.method = account.method;
                status.email = account.email;
                status.plan = account.plan;
                if status.method.as_deref() == Some("apiKey") {
                    status.key_ending = codex_key_ending(&self.env.home(Provider::Codex));
                }
            }
            Err(problem) => {
                status.problem.get_or_insert(problem);
            }
        }
        status
    }

    /// Starts a sign-in with a device code: the person enters the code at
    /// the address; Codex saves the credentials once they do.
    async fn codex_login(&self) -> Result<LoginStart, String> {
        let mut slot = self.device.lock().await;
        if let Some(login) = slot.take() {
            login.task.abort();
        }
        let mut server = self.env.codex_server().await?;
        let started = server
            .call(
                "account/login/start",
                json!({ "type": "chatgptDeviceCode" }),
            )
            .await;
        let started = match started {
            Ok(started) => started,
            Err(problem) => {
                server.stop().await;
                return Err(device_problem(&problem));
            }
        };
        let (Some(url), Some(code)) = (
            started["verificationUrl"].as_str().map(String::from),
            started["userCode"].as_str().map(String::from),
        ) else {
            server.stop().await;
            return Err("Codex did not give a sign-in code".into());
        };
        let outcome = Arc::new(std::sync::Mutex::new(None));
        let task = {
            let outcome = outcome.clone();
            tokio::spawn(async move {
                let done = server
                    .notification("account/login/completed", LOGIN_TIMEOUT)
                    .await;
                let result = match done {
                    Ok(params) if params["success"] == true => Ok(()),
                    Ok(params) => Err(params["error"]
                        .as_str()
                        .unwrap_or("the sign-in did not complete")
                        .to_string()),
                    Err(problem) => Err(problem),
                };
                *outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                server.stop().await;
            })
        };
        *slot = Some(DeviceLogin {
            url: url.clone(),
            code: code.clone(),
            started: std::time::Instant::now(),
            outcome,
            task,
        });
        Ok(LoginStart {
            url,
            code: Some(code),
            connected: false,
        })
    }

    async fn codex_key(&self, key: &str) -> Result<(), String> {
        let key = valid_openai_key(key)?.to_string();
        let checked = key.clone();
        tokio::task::spawn_blocking(move || check_openai_key(&checked))
            .await
            .map_err(|e| e.to_string())??;
        let _one = self.codex.lock().await;
        let mut server = self.env.codex_server().await?;
        let _ = server.call("account/logout", json!({})).await;
        let result = server
            .call(
                "account/login/start",
                json!({ "type": "apiKey", "apiKey": key }),
            )
            .await;
        server.stop().await;
        result
            .map(|_| ())
            .map_err(|problem| format!("Codex did not take the key: {problem}"))
    }

    async fn codex_logout(&self) -> Result<(), String> {
        *self.codex_models.lock().await = None;
        if let Some(login) = self.device.lock().await.take() {
            login.task.abort();
        }
        let _one = self.codex.lock().await;
        let mut server = self.env.codex_server().await?;
        let result = server.call("account/logout", json!({})).await;
        server.stop().await;
        result.map(|_| ())
    }
}

/// A refused device sign-in in plain words: ChatGPT accepts it only once
/// it is turned on in the account's security settings.
fn device_problem(problem: &str) -> String {
    let lower = problem.to_lowercase();
    if lower.contains("device") && (lower.contains("enable") || lower.contains("disabled")) {
        "sign-in with a code is off for this ChatGPT account: turn on device code sign-in in ChatGPT's security settings, then try again".into()
    } else {
        format!("the sign-in could not start: {problem}")
    }
}

/// An OpenAI key as OpenAI gives it: one word starting with `sk-`, not an
/// Anthropic key.
fn valid_openai_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("paste the API key".into());
    }
    if key.starts_with("sk-ant-") {
        return Err("this is an Anthropic key: ChatGPT needs an OpenAI API key".into());
    }
    if !key.starts_with("sk-")
        || key.len() > 512
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("this is not an OpenAI API key: it starts with sk-".into());
    }
    Ok(key)
}

/// Asks OpenAI whether the key works, with the list of models.
fn check_openai_key(key: &str) -> Result<(), String> {
    match ureq::get("https://api.openai.com/v1/models")
        .header("Authorization", &format!("Bearer {key}"))
        .call()
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(401 | 403)) => {
            Err("OpenAI did not accept this key: check that it is whole and still active".into())
        }
        Err(ureq::Error::StatusCode(code)) => Err(format!(
            "OpenAI could not check the key (status {code}): try again in a minute"
        )),
        Err(_) => Err("OpenAI could not be reached to check the key: try again in a minute".into()),
    }
}

/// The last four characters of the key Codex keeps, without reading the
/// rest of its credentials into memory longer than needed.
fn codex_key_ending(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join(".codex/auth.json")).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&text).ok()?;
    let key = auth["OPENAI_API_KEY"].as_str()?.trim();
    (key.len() > 12).then(|| key[key.len() - 4..].to_string())
}

/// The API key's file in the agent's home, and the command the CLI runs to
/// read it there.
const KEY_FILE: &str = ".claude/axioma-api-key";
const KEY_HELPER: &str = "cat /home/agent/.claude/axioma-api-key";

/// A key as Anthropic gives it: trimmed, one word, of a sensible length.
fn valid_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("paste the API key".into());
    }
    if !key.starts_with("sk-ant-")
        || key.len() > 256
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("this is not an Anthropic API key: it starts with sk-ant-".into());
    }
    Ok(key)
}

/// Asks Anthropic whether the key works, with the cheapest request there is:
/// the list of models.
fn check_key(key: &str) -> Result<(), String> {
    match ureq::get("https://api.anthropic.com/v1/models?limit=1")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .call()
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(401 | 403)) => {
            Err("Anthropic did not accept this key: check that it is whole and still active".into())
        }
        Err(ureq::Error::StatusCode(code)) => Err(format!(
            "Anthropic could not check the key (status {code}): try again in a minute"
        )),
        Err(_) => {
            Err("Anthropic could not be reached to check the key: try again in a minute".into())
        }
    }
}

/// Writes the key, readable only by its owner, and points the CLI's
/// settings at it.
fn keep_key(home: &std::path::Path, key: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = home.join(".claude");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let temporary = dir.join("axioma-api-key.new");
    let _ = std::fs::remove_file(&temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(key.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, home.join(KEY_FILE)).map_err(|e| e.to_string())?;
    edit_settings(home, |settings| {
        settings["apiKeyHelper"] = KEY_HELPER.into();
    })
}

/// Removes the key and the setting that reads it, if they are there.
fn forget_key(home: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(home.join(KEY_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    if home.join(".claude/settings.json").exists() {
        edit_settings(home, |settings| {
            if let Some(map) = settings.as_object_mut() {
                map.remove("apiKeyHelper");
            }
        })?;
    }
    Ok(())
}

/// Changes the CLI's own settings in the agent's home, keeping the rest.
fn edit_settings(
    home: &std::path::Path,
    change: impl FnOnce(&mut serde_json::Value),
) -> Result<(), String> {
    let path = home.join(".claude/settings.json");
    let mut settings: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    change(&mut settings);
    let temporary = home.join(".claude/settings.json.axioma");
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|e| e.to_string())
}

/// The last four characters of the key kept in `home`.
fn key_ending(home: &std::path::Path) -> Option<String> {
    let key = std::fs::read_to_string(home.join(KEY_FILE)).ok()?;
    let key = key.trim();
    (key.len() > 12).then(|| key[key.len() - 4..].to_string())
}

/// Drops sign-ins whose process ended or that waited longer than allowed.
pub fn start_login_reaper(account: Arc<Account>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(LOGIN_TIMEOUT / 15).await;
            account.expire_login().await;
        }
    });
}

/// An account that is never signed in, for tests of other modules.
#[cfg(test)]
pub fn idle() -> Arc<Account> {
    Arc::new(Account::new(
        Arc::new(Environment::new(
            PathBuf::from("/nonexistent/site"),
            PathBuf::from("/nonexistent/agent"),
            Runtimes {
                node: PathBuf::from("/nonexistent/node"),
                claude: PathBuf::from("/nonexistent/claude"),
                codex: None,
                managed: false,
                codex_managed: false,
            },
            None,
            PathBuf::from("/nonexistent/axioma"),
            PathBuf::from("/nonexistent/agent.sock"),
            PathBuf::from("/nonexistent/hooks.sock"),
            PathBuf::from("/nonexistent/policy"),
            None,
            Provider::Claude,
        )),
        PathBuf::from("/nonexistent"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_anthropic_keys_are_taken() {
        let key = "sk-ant-api03-abcDEF_123-xyz";
        assert_eq!(valid_key(&format!("  {key}\n")), Ok(key));
        assert!(valid_key("").unwrap_err().contains("paste"));
        assert!(valid_key("sk-proj-abc").unwrap_err().contains("sk-ant-"));
        assert!(valid_key("sk-ant-abc def").is_err());
        assert!(valid_key("sk-ant-abc\"; rm -rf /").is_err());
        assert!(valid_key(&format!("sk-ant-{}", "a".repeat(300))).is_err());
    }

    #[test]
    fn the_key_is_kept_for_the_cli_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), r#"{"theme":"dark"}"#).unwrap();

        keep_key(home, "sk-ant-api03-test-key-ABCD").unwrap();
        let file = home.join(KEY_FILE);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "sk-ant-api03-test-key-ABCD"
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(&file.metadata().unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["apiKeyHelper"], KEY_HELPER);
        assert_eq!(settings["theme"], "dark");
        assert_eq!(key_ending(home).as_deref(), Some("ABCD"));

        forget_key(home).unwrap();
        assert!(!file.exists());
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(settings.get("apiKeyHelper").is_none());
        assert_eq!(settings["theme"], "dark");
        // Nothing to forget: no error.
        forget_key(home).unwrap();
        assert_eq!(key_ending(home), None);
    }

    #[test]
    fn only_openai_keys_are_taken_for_gpt() {
        assert_eq!(
            valid_openai_key(" sk-proj-abc_DEF-1 "),
            Ok("sk-proj-abc_DEF-1")
        );
        assert!(
            valid_openai_key("sk-ant-api03-x")
                .unwrap_err()
                .contains("Anthropic")
        );
        assert!(valid_openai_key("xai-123").is_err());
        assert!(valid_openai_key("sk-a b").is_err());
    }

    #[test]
    fn a_single_home_becomes_claude_s() {
        let dir = tempfile::tempdir().unwrap();
        let homes = dir.path().join("agent");
        std::fs::create_dir_all(homes.join(".claude")).unwrap();
        std::fs::write(homes.join(".claude.json"), "{}").unwrap();
        separate_homes(&homes).unwrap();
        assert!(homes.join("claude/.claude.json").is_file());
        assert!(!homes.join(".claude.json").exists());
        // Once is enough.
        separate_homes(&homes).unwrap();
        assert!(homes.join("claude/.claude").is_dir());
        // A new installation has nothing to move.
        let fresh = dir.path().join("fresh");
        separate_homes(&fresh).unwrap();
        assert!(!fresh.exists());
    }
}
