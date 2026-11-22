//! git, run by damasco outside the sandbox on the site repository.
//!
//! The repository's working tree is written by the agent, so nothing in it
//! may make git run code here: hooks, fsmonitor, the system and global
//! configuration are switched off for every call. The agent itself cannot
//! touch `.git`, which is read-only in its sandbox.
//!
//! A repository inside the site would bring a configuration of its own,
//! and git reads one by starting another git in it. So git finds no program
//! to start (`no_children`), which holds whatever is put in the files and
//! whenever; what damasco asks of git needs none, and the few calls that do
//! (`run_alone`) read nothing of the working tree. Such repositories are
//! also set aside before a save, so that their files are the site's like
//! the others.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const AUTHOR: &str = "damasco";
const EMAIL: &str = "damasco@localhost";
/// Nothing can be made under it: a folder with no programs.
const NO_PROGRAMS: &str = "/dev/null/none";

/// git itself, found once in damasco's PATH.
fn program() -> Result<&'static Path, String> {
    static GIT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT.get_or_init(|| {
        let found = crate::check::find_in_path(std::env::var_os("PATH").as_ref(), "git")?;
        std::path::absolute(found).ok()
    })
    .as_deref()
    .ok_or_else(|| "git: not found in PATH".to_string())
}

/// The environment and settings of every call.
fn base(repo: &Path) -> Result<Command, String> {
    let mut command = Command::new(program()?);
    command
        .current_dir(repo)
        .env_clear()
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
            "-c",
            "submodule.recurse=false",
        ]);
    Ok(command)
}

/// A git that cannot start another program: neither itself in a repository
/// nested in the site, nor what that repository's configuration names.
fn no_children(repo: &Path) -> Result<Command, String> {
    let mut command = base(repo)?;
    command
        .env("PATH", NO_PROGRAMS)
        .env("GIT_EXEC_PATH", NO_PROGRAMS)
        // They are programs: `commit_all` asks for the housekeeping itself.
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"]);
    Ok(command)
}

/// Runs a git that may start others, only for what does not look at the
/// working tree: making and pruning worktrees, packing objects.
pub fn run_alone(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = base(repo)?
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["-c", "gc.autoDetach=false"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("git: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {}: {}", args.join(" "), stderr.trim()))
    }
}

/// Runs `git args…` in `repo` and returns its standard output.
pub fn run(repo: &Path, args: &[&str]) -> Result<String, String> {
    run_with(repo, args, None, None)
}

/// Like `run`, with another index file than the repository's (to compose
/// a tree without touching the working tree) and text on standard input.
pub fn run_with(
    repo: &Path,
    args: &[&str],
    index: Option<&Path>,
    input: Option<&str>,
) -> Result<String, String> {
    use std::io::Write;
    let mut command = no_children(repo)?;
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command
        .args(args)
        .stdin(if input.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("git: {err}"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        let input = input.to_string();
        // Written from a thread, so that a long answer cannot block it.
        std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("git: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {}: {}", args.join(" "), stderr.trim()))
    }
}

/// The contents of several files at given commits (`<commit>:<path>`), read
/// by one git process; `None` for a file that is not there.
pub fn show_many(repo: &Path, specs: &[String]) -> Result<Vec<Option<Vec<u8>>>, String> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    use std::io::Write;
    let mut child = no_children(repo)?
        .args(["cat-file", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| format!("git: {err}"))?;
    let mut input = String::new();
    for spec in specs {
        if spec.contains('\n') {
            return Err("git cat-file: a path with a line break".into());
        }
        input.push_str(spec);
        input.push('\n');
    }
    // Written from a thread: git answers while it reads, and a large answer
    // would otherwise fill the pipe before the input is all written.
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child
        .wait_with_output()
        .map_err(|err| format!("git cat-file: {err}"))?;
    let _ = writer.join();
    let out = output.stdout;
    let mut files = Vec::with_capacity(specs.len());
    let mut at = 0;
    for _ in specs {
        let end = out[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| at + i)
            .ok_or("git cat-file: the answer ended early")?;
        let header = String::from_utf8_lossy(&out[at..end]).into_owned();
        at = end + 1;
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            files.push(None);
            continue;
        }
        // "<object> <type> <size>", then the contents and a line break.
        let size: usize = header
            .rsplit(' ')
            .next()
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| format!("git cat-file: {header}"))?;
        let contents = out
            .get(at..at + size)
            .ok_or("git cat-file: the answer ended early")?;
        files.push(header.contains(" blob ").then(|| contents.to_vec()));
        at += size + 1;
    }
    Ok(files)
}

/// Records every change of the working tree in a commit, except the files
/// damasco generates in `.damasco/`. Returns the new commit, or `None` when
/// nothing changed.
pub fn commit_all(repo: &Path, message: &str) -> Result<Option<String>, String> {
    set_nested_aside(repo).map_err(|err| format!("the site's folders cannot be read: {err}"))?;
    forget_gitlinks(repo)?;
    run(repo, &["add", "-A", "--", ".", ":(exclude).damasco"])?;
    // One made meanwhile is no part of the version.
    forget_gitlinks(repo)?;
    if run(repo, &["diff", "--cached", "--name-only"])?
        .trim()
        .is_empty()
    {
        return Ok(None);
    }
    run(repo, &["commit", "-q", "-m", message])?;
    let commit = run(repo, &["rev-parse", "HEAD"])?.trim().to_string();
    if let Err(err) = run_alone(repo, &["gc", "--auto", "--quiet"]) {
        eprintln!("damasco: the history is not packed: {err}");
    }
    Ok(Some(commit))
}

/// Moves the `.git` of repositories nested in the site to `nested-git/`,
/// next to it: their files are then saved as the site's, and what the
/// repositories held is not lost.
fn set_nested_aside(repo: &Path) -> std::io::Result<()> {
    use crate::confined::Dir;
    let Some(root) = repo.parent() else {
        return Ok(());
    };
    let aside = root.join("nested-git");
    let top = Dir::root(repo)?;
    let mut seen = std::collections::HashSet::from([top.identity()?]);
    // By path, not by open folder: a deep or wide tree would take every
    // descriptor there is.
    let mut waiting = vec![PathBuf::new()];
    while let Some(path) = waiting.pop() {
        // Gone, or swapped for something else: nothing of ours.
        let Ok(dir) = crate::confined::dir(repo, &path) else {
            continue;
        };
        let here = path.as_os_str().is_empty();
        if !here && !seen.insert(dir.identity()?) {
            continue;
        }
        for (name, kind) in dir.entries().unwrap_or_default() {
            if name == ".git" {
                if here {
                    continue;
                }
                let target = aside.join(crate::site::ulid());
                let moved =
                    std::fs::create_dir_all(&aside).and_then(|()| dir.move_out(&name, &target));
                match moved {
                    Ok(()) => eprintln!(
                        "damasco: a repository inside the site, in {}, is set aside in {}",
                        path.display(),
                        target.display()
                    ),
                    // It stays and runs nothing: the save goes on.
                    Err(err) => eprintln!(
                        "damasco: a repository inside the site, in {}, cannot be set aside: {err}",
                        path.display()
                    ),
                }
            } else if kind.is_dir() && name != "node_modules" {
                waiting.push(path.join(&name));
            }
        }
    }
    Ok(())
}

/// Takes out of the index the entries that stand for other repositories:
/// git looks into those.
fn forget_gitlinks(repo: &Path) -> Result<(), String> {
    let staged = run(repo, &["ls-files", "-s", "-z"])?;
    let gone: String = staged
        .split('\0')
        .filter(|entry| entry.starts_with("160000 "))
        .filter_map(|entry| entry.split_once('\t'))
        .map(|(_, path)| format!("{path}\0"))
        .collect();
    if !gone.is_empty() {
        run_with(
            repo,
            &["update-index", "--force-remove", "-z", "--stdin"],
            None,
            Some(&gone),
        )?;
    }
    Ok(())
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

    /// A site with a repository inside that names a command for git to run,
    /// recorded in the site's history as older versions did.
    fn with_nested(dir: &Path) -> (PathBuf, PathBuf) {
        let (site, ran) = (dir.join("site"), dir.join("ran"));
        let inner = site.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        run(&site, &["init", "-q", "-b", "master"]).unwrap();
        run(&inner, &["init", "-q"]).unwrap();
        std::fs::write(inner.join("file"), "x").unwrap();
        run(&inner, &["add", "file"]).unwrap();
        run(&inner, &["commit", "-q", "-m", "inner"]).unwrap();
        run(&site, &["add", "-A"]).unwrap();
        run(&site, &["commit", "-q", "-m", "site"]).unwrap();
        let clean = format!("touch {}; cat", ran.display());
        run(&inner, &["config", "filter.probe.clean", &clean]).unwrap();
        std::fs::write(inner.join(".gitattributes"), "* filter=probe\n").unwrap();
        // The same size at another time: git would read the file to compare.
        let touched = std::fs::File::options()
            .write(true)
            .open(inner.join("file"))
            .unwrap();
        touched
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        (site, ran)
    }

    #[test]
    fn a_repository_inside_the_site_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (site, ran) = with_nested(dir.path());
        // Whatever is asked, and before anything is set aside: with the
        // look into submodules asked for outright, too.
        for args in [
            &["status", "--porcelain"][..],
            &["status", "--porcelain", "--ignore-submodules=none"],
            &["diff", "--ignore-submodules=none", "--name-only"],
            &["add", "-A"],
            &["commit", "-q", "-m", "again"],
        ] {
            let _ = run(&site, args);
            assert!(!ran.exists(), "{args:?}");
        }
        // What damasco asks still works while one is there.
        assert!(run(&site, &["status", "--porcelain", "--ignore-submodules=all"]).is_ok());

        // And a version that recorded one still builds apart, the way a
        // selection does: made, then used again.
        let older = run(&site, &["rev-list", "--max-parents=0", "HEAD"]).unwrap();
        let work = dir.path().join("work");
        let path = work.to_string_lossy().into_owned();
        let add = [
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            &path,
            older.trim(),
        ];
        run_alone(&site, &add).unwrap();
        run(&work, &["read-tree", "-u", "--reset", older.trim()]).unwrap();
        run(&work, &["checkout", "-q", "--detach", "-f", older.trim()]).unwrap();
        run(&work, &["clean", "-fdq"]).unwrap();
        assert!(!ran.exists());
    }

    #[test]
    fn a_repository_inside_the_site_becomes_files_of_the_site() {
        let dir = tempfile::tempdir().unwrap();
        let (site, ran) = with_nested(dir.path());
        commit_all(&site, "Saved").unwrap().unwrap();
        assert!(!ran.exists());
        assert!(!site.join("inner/.git").exists());
        let files = run(&site, &["ls-tree", "-r", "HEAD"]).unwrap();
        assert!(
            files.contains("inner/file") && !files.contains("160000"),
            "{files}"
        );
        assert!(run(&site, &["status", "--porcelain"]).unwrap().is_empty());
        let kept: Vec<_> = std::fs::read_dir(dir.path().join("nested-git"))
            .unwrap()
            .collect();
        assert_eq!(kept.len(), 1);

        // Going back to a version that recorded one runs nothing either,
        // and saves no such entry again.
        let older = run(&site, &["rev-parse", "HEAD~1"]).unwrap();
        restore(&site, older.trim()).unwrap();
        commit_all(&site, "Went back").unwrap();
        assert!(!ran.exists());
        assert!(!run(&site, &["ls-files", "-s"]).unwrap().contains("160000"));

        // One without a commit, which git refuses to add, saves the same.
        std::fs::create_dir_all(site.join("a/b")).unwrap();
        run(&site.join("a/b"), &["init", "-q"]).unwrap();
        std::fs::write(site.join("a/b/new"), "x").unwrap();
        assert!(commit_all(&site, "Saved again").unwrap().is_some());
        assert!(run(&site, &["ls-files"]).unwrap().contains("a/b/new"));
    }

    #[test]
    fn taking_a_nested_repository_out_is_a_change_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let (site, _ran) = with_nested(dir.path());
        std::fs::remove_dir_all(site.join("inner")).unwrap();
        assert!(commit_all(&site, "Removed").unwrap().is_some());
        let changed = run(&site, &["diff", "--name-status", "HEAD~1", "HEAD"]).unwrap();
        assert!(changed.contains("inner"), "{changed}");
    }

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
        assert_eq!(author.trim(), "damasco <damasco@localhost>");
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
        std::fs::create_dir(repo.join(".damasco")).unwrap();
        std::fs::write(repo.join(".damasco/schema.ts"), "generated").unwrap();
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
