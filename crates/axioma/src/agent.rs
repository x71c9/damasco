//! The coding agent: the official Claude Code CLI, run in the agent sandbox
//! with the account of the site's owner.
//!
//! This module covers the account: whether the CLI is signed in, and the
//! sign-in itself. `claude auth login` prints the address of Anthropic's
//! sign-in page and waits for the code that page shows at the end; the panel
//! opens the address and sends the code back. The credentials stay in the
//! agent's home, inside the sandbox.

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
            }),
            policy: Some(&self.policy),
            cgroup: self.cgroup.as_deref(),
        })
    }

    /// A command of the CLI in the sandbox, with piped input and output.
    fn claude(
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

/// What the panel shows about the agent's account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AccountStatus {
    pub signed_in: bool,
    /// "claude.ai" for a subscription, as the CLI reports it.
    pub method: Option<String>,
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

    pub async fn logout(&self) -> Result<(), String> {
        *self.login.lock().await = None;
        let (mut command, _filter) = self.env.claude(&["auth", "logout"])?;
        let output = command.output().await.map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
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
        policy: PathBuf::from("/nonexistent/policy"),
        cgroup: None,
    })))
}
