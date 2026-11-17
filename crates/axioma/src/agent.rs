//! The coding agent: the official Claude Code CLI, run in the agent sandbox
//! with the account of the site's owner.
//!
//! This module covers the account: whether the CLI is signed in, and the
//! sign-in itself. `claude auth login` prints the address of Anthropic's
//! sign-in page and waits for the code that page shows at the end; the panel
//! opens the address and sends the code back. The credentials stay in the
//! agent's home, inside the sandbox.
//!
//! An Anthropic API key is the other way: axioma checks it with Anthropic,
//! keeps it in a file of the agent's home and has the CLI read it through
//! `apiKeyHelper`, so it never appears in a command line or in the
//! environment of other processes. One way at a time: the key replaces the
//! subscription's sign-in, and disconnecting removes both.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;

use crate::runtime::Runtimes;
use crate::sandbox::{Egress, Profile, Sandbox};

/// Everything the agent's sandbox needs.
pub struct Environment {
    pub site: PathBuf,
    /// The agent's persistent home: sessions, memory, credentials.
    pub home: PathBuf,
    pub runtimes: Runtimes,
    pub bwrap: Option<PathBuf>,
    pub axioma: PathBuf,
    /// Socket of the egress proxy with the agent allowlist.
    pub proxy: PathBuf,
    /// Socket where the chat answers the hooks of the CLI.
    pub hooks: PathBuf,
    /// Directory with the product rules, mounted at /etc/claude-code.
    pub policy: PathBuf,
    pub cgroup: Option<PathBuf>,
}

impl Environment {
    fn sandbox(&self) -> Result<Sandbox<'_>, String> {
        let bwrap = self.bwrap.clone().ok_or("bwrap is not installed")?;
        if !self.runtimes.missing().is_empty() {
            return Err("the assistant is still being downloaded".into());
        }
        Ok(Sandbox {
            profile: Profile::Agent,
            bwrap,
            site: &self.site,
            agent_home: &self.home,
            runtimes: &self.runtimes,
            egress: Some(Egress {
                axioma: &self.axioma,
                socket: &self.proxy,
                hooks: Some(self.hooks.as_path()).filter(|path| path.exists()),
            }),
            policy: Some(&self.policy),
            cgroup: self.cgroup.as_deref(),
        })
    }

    /// A command of the CLI in the sandbox, with piped input and output.
    /// The returned descriptor holds the system call filter: keep it open
    /// until the command is spawned.
    pub fn claude(
        &self,
        args: &[&str],
    ) -> Result<(tokio::process::Command, std::os::fd::OwnedFd), String> {
        let sandbox = self.sandbox()?;
        let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
        let (command, filter) = sandbox
            .command("claude", &args)
            .map_err(|e| e.to_string())?
            .into_parts();
        let mut command = tokio::process::Command::from(command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok((command, filter))
    }
}

impl Environment {
    /// Prepares the CLI's settings in the agent's home for the interactive
    /// mode: the first-run screens are done, `/site` is trusted (axioma
    /// chose it) and the text style follows the panel's `theme`. Other
    /// settings stay as the CLI wrote them.
    pub fn prepare_terminal(&self, theme: &str) -> Result<(), String> {
        let path = self.home.join(".claude.json");
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
        let temporary = self.home.join(".claude.json.axioma");
        let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
        std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
        std::fs::set_permissions(
            &temporary,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &path).map_err(|e| e.to_string())
    }

    /// The interactive CLI in the sandbox, for a terminal): the caller
    /// attaches it. The descriptor holds the system call filter until the
    /// command is spawned.
    pub fn claude_terminal(
        &self,
        args: &[&str],
    ) -> Result<(std::process::Command, std::os::fd::OwnedFd), String> {
        let sandbox = self.sandbox()?;
        let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
        Ok(sandbox
            .terminal_command("claude", &args)
            .map_err(|e| e.to_string())?
            .into_parts())
    }
}

/// What the panel shows about the agent's account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AccountStatus {
    pub signed_in: bool,
    /// "claude.ai" for a subscription, "api_key_helper" for an API key, as
    /// the CLI reports it.
    pub method: Option<String>,
    /// The last characters of the API key, to tell which key it is.
    pub key_ending: Option<String>,
    /// When the sign-in expires, as the CLI reports it.
    pub expires_at: Option<String>,
    /// A sign-in is waiting for its code.
    pub signing_in: bool,
    /// Why the account cannot be checked now, if so.
    pub problem: Option<String>,
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

pub struct Account {
    env: Arc<Environment>,
    login: Mutex<Option<Login>>,
}

/// How long a sign-in waits for its code.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

impl Account {
    pub fn new(env: Arc<Environment>) -> Self {
        Self {
            env,
            login: Mutex::new(None),
        }
    }

    /// The account as the CLI reports it, without looking at a sign-in in
    /// progress (safe to call while holding the sign-in).
    async fn status_now(&self) -> AccountStatus {
        self.read_status(false).await
    }

    pub fn environment(&self) -> Arc<Environment> {
        self.env.clone()
    }

    pub async fn status(&self) -> AccountStatus {
        let signing_in = self.login.lock().await.is_some();
        self.read_status(signing_in).await
    }

    async fn read_status(&self, signing_in: bool) -> AccountStatus {
        let output = match self.env.claude(&["auth", "status", "--json"]) {
            Ok((mut command, _filter)) => command.output().await.map_err(|e| e.to_string()),
            Err(problem) => Err(problem),
        };
        let parsed = output.and_then(|output| {
            serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .map_err(|_| String::from_utf8_lossy(&output.stderr).trim().to_string())
        });
        match parsed {
            Ok(json) => AccountStatus {
                signed_in: json["loggedIn"].as_bool().unwrap_or(false),
                method: json["authMethod"]
                    .as_str()
                    .filter(|m| *m != "none")
                    .map(String::from),
                key_ending: (json["authMethod"] == "api_key_helper")
                    .then(|| key_ending(&self.env.home))
                    .flatten(),
                expires_at: json["loginExpiresAt"].as_str().map(String::from),
                signing_in,
                problem: None,
            },
            Err(problem) => AccountStatus {
                signing_in,
                problem: Some(problem),
                ..AccountStatus::default()
            },
        }
    }

    /// Starts `claude auth login` and returns the address of the sign-in
    /// page. A sign-in already waiting is replaced.
    pub async fn start_login(&self) -> Result<String, String> {
        let mut slot = self.login.lock().await;
        *slot = None;
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

    /// Sends the code shown by the sign-in page. A code the CLI refuses
    /// leaves the sign-in open, so that another code can be sent.
    pub async fn finish_login(&self, code: &str) -> Result<(), String> {
        let mut slot = self.login.lock().await;
        let Some(mut login) = slot.take() else {
            return Err("no sign-in is waiting for a code; start again".into());
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
                        if self.status_now().await.signed_in {
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
                if self.status_now().await.signed_in {
                    Ok(())
                } else if outcome.is_err() {
                    Err("the sign-in did not answer; start it again".into())
                } else {
                    Err("the sign-in failed; start it again".into())
                }
            }
        }
    }

    /// Stops a sign-in that has been waiting too long.
    pub async fn expire_login(&self) {
        let mut slot = self.login.lock().await;
        if let Some(login) = slot.as_mut()
            && (login.child.try_wait().ok().flatten().is_some()
                || login.started.elapsed() > LOGIN_TIMEOUT)
        {
            *slot = None;
        }
    }

    /// Connects the agent with an Anthropic API key, once Anthropic accepts
    /// it. The subscription's sign-in, if any, is removed.
    pub async fn use_key(&self, key: &str) -> Result<(), String> {
        let key = valid_key(key)?.to_string();
        let checked = key.clone();
        tokio::task::spawn_blocking(move || check_key(&checked))
            .await
            .map_err(|e| e.to_string())??;
        // Not signed in with a subscription is fine too.
        let _ = self.logout().await;
        keep_key(&self.env.home, &key)?;
        if self.status_now().await.signed_in {
            Ok(())
        } else {
            forget_key(&self.env.home)?;
            Err("the assistant could not use the key".into())
        }
    }

    pub async fn logout(&self) -> Result<(), String> {
        *self.login.lock().await = None;
        forget_key(&self.env.home)?;
        let (mut command, _filter) = self.env.claude(&["auth", "logout"])?;
        let output = command.output().await.map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
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
    Arc::new(Account::new(Arc::new(Environment {
        site: PathBuf::from("/nonexistent/site"),
        home: PathBuf::from("/nonexistent/agent"),
        runtimes: Runtimes {
            node: PathBuf::from("/nonexistent/node"),
            claude: PathBuf::from("/nonexistent/claude"),
            managed: false,
        },
        bwrap: None,
        axioma: PathBuf::from("/nonexistent/axioma"),
        proxy: PathBuf::from("/nonexistent/agent.sock"),
        hooks: PathBuf::from("/nonexistent/hooks.sock"),
        policy: PathBuf::from("/nonexistent/policy"),
        cgroup: None,
    })))
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
}
