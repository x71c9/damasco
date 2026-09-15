//! git, run by axioma outside the sandbox on the site repository.
//!
//! The repository's working tree is written by the agent, so nothing in it
//! may make git run code here: hooks, fsmonitor, the system and global
//! configuration are switched off for every call. The agent itself cannot
//! touch `.git`, which is read-only in its sandbox.

use std::path::Path;
use std::process::Command;

const AUTHOR: &str = "axioma";
const EMAIL: &str = "axioma@localhost";

/// Runs `git args…` in `repo` and returns its standard output.
pub fn run(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(repo)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", AUTHOR)
        .env("GIT_AUTHOR_EMAIL", EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR)
        .env("GIT_COMMITTER_EMAIL", EMAIL)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.allow=never",
        ])
        .args(args)
        .output()
        .map_err(|err| format!("git: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {}: {}", args.join(" "), stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_hooks_written_in_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run(repo, &["init", "-q", "-b", "master"]).unwrap();
        let marker = repo.join("hook-ran");
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        std::fs::write(repo.join("file"), "x").unwrap();
        run(repo, &["add", "-A"]).unwrap();
        run(repo, &["commit", "-q", "-m", "test"]).unwrap();
        assert!(!marker.exists(), "the repository's hook must not run");
        let author = run(repo, &["log", "-1", "--format=%an <%ae>"]).unwrap();
        assert_eq!(author.trim(), "axioma <axioma@localhost>");
    }
}
