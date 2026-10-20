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

/// Records every change of the working tree in a commit, except the files
/// axioma generates in `.axioma/`. Returns the new commit, or `None` when
/// nothing changed.
pub fn commit_all(repo: &Path, message: &str) -> Result<Option<String>, String> {
    run(repo, &["add", "-A", "--", ".", ":(exclude).axioma"])?;
    if run(repo, &["diff", "--cached", "--name-only"])?
        .trim()
        .is_empty()
    {
        return Ok(None);
    }
    run(repo, &["commit", "-q", "-m", message])?;
    Ok(Some(run(repo, &["rev-parse", "HEAD"])?.trim().to_string()))
}

/// The current commit.
pub fn head(repo: &Path) -> Result<String, String> {
    Ok(run(repo, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// Whether two commits hold the same files.
pub fn same_tree(repo: &Path, a: &str, b: &str) -> Result<bool, String> {
    let tree = |commit: &str| run(repo, &["rev-parse", &format!("{commit}^{{tree}}")]);
    Ok(tree(a)?.trim() == tree(b)?.trim())
}

/// Puts the files of `commit` in the working tree: changed files go back,
/// added ones go away. Untracked and ignored files stay. The history is
/// kept: the caller records the result in a new commit.
pub fn restore(repo: &Path, commit: &str) -> Result<(), String> {
    run(repo, &["read-tree", "-u", "--reset", commit])?;
    Ok(())
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

    #[test]
    fn restores_an_earlier_commit_as_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run(repo, &["init", "-q", "-b", "master"]).unwrap();
        std::fs::write(repo.join("page"), "one").unwrap();
        let first = commit_all(repo, "First").unwrap().unwrap();
        std::fs::write(repo.join("page"), "two").unwrap();
        std::fs::write(repo.join("added"), "x").unwrap();
        let second = commit_all(repo, "Second").unwrap().unwrap();
        assert!(!same_tree(repo, &first, &second).unwrap());
        restore(repo, &first).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("page")).unwrap(), "one");
        assert!(!repo.join("added").exists());
        let back = commit_all(repo, "Back").unwrap().unwrap();
        assert!(same_tree(repo, &first, &back).unwrap());
        // And forward again: nothing was lost.
        restore(repo, &second).unwrap();
        commit_all(repo, "Forward").unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("added")).unwrap(), "x");
        assert_eq!(head(repo).unwrap().len(), 40);
    }

    #[test]
    fn commits_changes_but_not_generated_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run(repo, &["init", "-q", "-b", "master"]).unwrap();
        std::fs::write(repo.join("page"), "one").unwrap();
        let first = commit_all(repo, "First").unwrap().unwrap();
        assert_eq!(commit_all(repo, "Nothing").unwrap(), None);
        std::fs::create_dir(repo.join(".axioma")).unwrap();
        std::fs::write(repo.join(".axioma/schema.ts"), "generated").unwrap();
        assert_eq!(commit_all(repo, "Only generated").unwrap(), None);
        std::fs::write(repo.join("page"), "two").unwrap();
        std::fs::write(repo.join("new"), "x").unwrap();
        let second = commit_all(repo, "Change the page\n\nAsked in the chat.")
            .unwrap()
            .unwrap();
        assert_ne!(first, second);
        let files = run(repo, &["show", "--name-only", "--format=%s", "HEAD"]).unwrap();
        assert_eq!(
            files.split_whitespace().collect::<Vec<_>>(),
            ["Change", "the", "page", "new", "page"]
        );
    }
}
