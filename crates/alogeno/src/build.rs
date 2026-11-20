//! Builds of the site: `npm ci` when the lockfile changes, then `npm run
//! build` in the build sandbox. Each successful build becomes an immutable
//! release in `data_dir/releases/<id>`, and `releases/preview` points to the
//! latest one. Publishing points `releases/live` to the release of the
//! preview, and `releases/previous` to the one that was live before. Builds run one at a time; requests that arrive during a build
//! or within a short delay are merged into the next one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, watch};

use crate::runtime::Runtimes;
use crate::sandbox::{Egress, Profile, Sandbox};
use crate::{git, site};

/// Releases kept on disk, besides the ones in use.
const KEEP: usize = 10;
/// Requests closer than this to each other produce one build.
const DEBOUNCE: Duration = Duration::from_millis(400);
/// End of the build output kept with the status.
const LOG_TAIL: usize = 8 * 1024;

/// Everything a build needs.
#[derive(Clone)]
pub struct Environment {
    pub data_dir: PathBuf,
    /// The files to build: the site, or a selection of its changes.
    pub site: PathBuf,
    pub runtimes: Runtimes,
    pub bwrap: Option<PathBuf>,
    pub alogeno: PathBuf,
    /// Socket of the egress proxy with the build allowlist.
    pub proxy: PathBuf,
    /// `cgroup.procs` of the build group, when limits are available.
    pub cgroup: Option<PathBuf>,
}

impl Environment {
    pub fn releases(&self) -> PathBuf {
        self.data_dir.join("releases")
    }
}

/// What the panel shows about builds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// "idle", "running", "ready" or "failed".
    pub state: &'static str,
    /// Release shown by the preview, if any build ever succeeded.
    pub release: Option<String>,
    /// Why the last build failed, in one sentence.
    pub error: Option<String>,
    /// End of the output of the last build.
    pub log: Option<String>,
    /// Requests the last finished build includes: a request numbered n is
    /// done once this reaches n.
    #[serde(skip)]
    pub covers: u64,
}

pub struct Builder {
    environment: Environment,
    pending: AtomicBool,
    /// Requests received so far.
    requests: AtomicU64,
    wake: Notify,
    status: watch::Sender<Status>,
    /// One build of a selection at a time.
    selection: tokio::sync::Mutex<()>,
    /// Set when the site is deleted: the worker ends.
    stopped: AtomicBool,
}

impl Builder {
    /// Starts the worker that runs builds in the background.
    pub fn start(environment: Environment) -> Arc<Self> {
        let release = current_release(&environment.releases());
        let state = if release.is_some() { "ready" } else { "idle" };
        let (status, _) = watch::channel(Status {
            state,
            release,
            error: None,
            log: None,
            covers: 0,
        });
        let builder = Arc::new(Self {
            environment,
            pending: AtomicBool::new(false),
            requests: AtomicU64::new(0),
            wake: Notify::new(),
            status,
            selection: tokio::sync::Mutex::new(()),
            stopped: AtomicBool::new(false),
        });
        tokio::spawn(builder.clone().work());
        // A preview that does not show the last saved version (a restart
        // after changes, an older alogeno) cannot be published: it is built
        // again.
        let env = &builder.environment;
        // The generated files follow this version of alogeno at once: the
        // assistant may read them before the next build.
        if let Ok(schema) = crate::schema::Schema::load(&env.site)
            && env.site.join(".git").is_dir()
        {
            let _ = write_generated(&env.site, &schema);
        }
        // The untouched template has nothing to show: the preview shows the
        // empty page, also where an earlier alogeno built the template.
        let untouched = crate::site::untouched(&env.site);
        if untouched {
            let _ = std::fs::remove_file(env.releases().join("preview"));
            builder.status.send_modify(|s| {
                s.state = "idle";
                s.release = None;
            });
        }
        if !untouched
            && builder.runtimes_ready()
            && env.site.join(".git").is_dir()
            && crate::publish::ready(&env.site, &env.releases()).is_err()
        {
            builder.request();
        }
        builder
    }

    /// Whether alogeno builds on its own, without a change asking for it: at
    /// start, and when the runtimes finish downloading. Not while the
    /// runtimes are missing (the download asks for a build when it ends),
    /// nor while the site is the untouched template (the first change does).
    pub fn builds_by_itself(&self) -> bool {
        self.runtimes_ready() && !crate::site::untouched(&self.environment.site)
    }

    fn runtimes_ready(&self) -> bool {
        self.environment.runtimes.missing(None).is_empty()
    }

    /// Asks for a build of the site as it is now; returns the number of the
    /// request.
    pub fn request(&self) -> u64 {
        let number = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        self.pending.store(true, Ordering::SeqCst);
        self.wake.notify_one();
        number
    }

    /// Asks for a build and waits for a build that includes the site as it
    /// is now, successful or not.
    pub async fn build_and_wait(&self) -> Status {
        let mut status = self.status.subscribe();
        let number = self.request();
        match status.wait_for(|s| s.covers >= number).await {
            Ok(status) => status.clone(),
            Err(_) => self.status(),
        }
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn releases(&self) -> PathBuf {
        self.environment.releases()
    }

    /// A folder of alogeno's own for work on the site, such as checks.
    pub fn work_dir(&self, name: &str) -> PathBuf {
        self.environment.data_dir.join("build").join(name)
    }

    /// Builds `commit` of the site in a folder of its own (a git worktree
    /// kept between builds, with its own dependencies), apart from the
    /// preview and the assistant's work. Returns the release, which nothing
    /// points to yet.
    pub async fn build_commit(&self, commit: &str) -> Result<String, String> {
        let _one_at_a_time = self.selection.lock().await;
        let site = self.environment.site.clone();
        let dir = self.work_dir("selection");
        let target = commit.to_string();
        let ready = dir.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            if !ready.join(".git").exists() {
                let _ = std::fs::remove_dir_all(&ready);
                git::run(&site, &["worktree", "prune"])?;
                let path = ready.to_string_lossy().into_owned();
                git::run(
                    &site,
                    &["worktree", "add", "--detach", "--force", &path, &target],
                )?;
            } else {
                git::run(&ready, &["checkout", "-q", "--detach", "-f", &target])?;
                git::run(&ready, &["clean", "-fdq"])?;
            }
            Ok(())
        })
        .await
        .map_err(|err| err.to_string())??;
        let mut env = self.environment.clone();
        env.site = dir;
        build_release(&env)
            .await
            .map(|(release, _)| release)
            .map_err(|(error, _)| error)
    }

    /// Ends the worker, when the site is deleted. A build in progress ends
    /// first.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Whether a build is running now.
    pub fn running(&self) -> bool {
        self.status.borrow().state == "running"
    }

    async fn work(self: Arc<Self>) {
        loop {
            self.wake.notified().await;
            if self.stopped.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(DEBOUNCE).await;
            while self.pending.swap(false, Ordering::SeqCst) {
                // Every request received until now sees the files this build
                // reads.
                let covers = self.requests.load(Ordering::SeqCst);
                self.status.send_modify(|s| {
                    s.state = "running";
                    s.error = None;
                });
                let started = Instant::now();
                let outcome = build(&self.environment).await;
                let seconds = started.elapsed().as_secs_f32();
                self.status.send_modify(|s| {
                    s.covers = covers;
                    match outcome {
                        Ok((release, log)) => {
                            eprintln!("alogeno: build {release} ready in {seconds:.1}s");
                            s.state = "ready";
                            s.release = Some(release);
                            s.log = Some(log);
                        }
                        Err((error, log)) => {
                            eprintln!("alogeno: build failed: {error}");
                            s.state = "failed";
                            s.error = Some(error);
                            s.log = Some(log);
                        }
                    }
                });
            }
        }
    }
}

/// The release `releases/preview` points to.
pub fn current_release(releases: &Path) -> Option<String> {
    pointed(releases, "preview")
}

/// The release `releases/<name>` points to: preview, live or previous.
pub fn pointed(releases: &Path, name: &str) -> Option<String> {
    let target = std::fs::read_link(releases.join(name)).ok()?;
    target.file_name()?.to_str().map(String::from)
}

/// Writes the files alogeno generates for the site in `.alogeno/`: the Zod
/// schema of pages, pictures and components.
pub fn write_generated(site: &Path, schema: &crate::schema::Schema) -> Result<(), String> {
    let generated = site.join(".alogeno");
    let text = crate::schema::zod(schema);
    let path = generated.join("schema.ts");
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::create_dir_all(&generated)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|e| format!("writing .alogeno/schema.ts: {e}"))
}

type Failure = (String, String);

/// One build of the site, shown by the preview: returns the new release
/// and the end of the output, or the reason it failed and the end of the
/// output.
pub async fn build(env: &Environment) -> Result<(String, String), Failure> {
    let (release, log) = build_release(env).await?;
    let releases = env.releases();
    point(&releases, "preview", &release)
        .map_err(|e| (format!("switching the preview: {e}"), log.clone()))?;
    prune(&releases);
    Ok((release, log))
}

/// One build of the files in `env.site` into a new sealed release, which
/// nothing points to yet.
pub async fn build_release(env: &Environment) -> Result<(String, String), Failure> {
    let mut log = String::new();
    let fail = |error: String, log: &str| (error, tail(log));
    let Some(bwrap) = env.bwrap.clone() else {
        return Err(fail("bwrap is not installed".into(), &log));
    };
    if !env.runtimes.missing(None).is_empty() {
        return Err(fail("the runtimes are still being downloaded".into(), &log));
    }
    // One build at a time across every site; every build, the preview's
    // and a selection's, comes through here once.
    let _one_build = crate::gate::BUILDS
        .acquire()
        .await
        .map_err(|err| fail(err.to_string(), &log))?;
    let sandbox = Sandbox {
        profile: Profile::Build,
        bwrap,
        site: &env.site,
        agent_home: &env.site,
        runtimes: &env.runtimes,
        egress: Some(Egress {
            alogeno: &env.alogeno,
            socket: &env.proxy,
            hooks: None,
        }),
        cli: None,
        cgroup: env.cgroup.as_deref(),
    };

    // The pages must match their types before Astro reads them; the schema
    // Astro reads them with is generated here, outside the sandbox.
    let schema = crate::schema::Schema::load(&env.site).map_err(|found| {
        let list = found
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        log.push_str(&list);
        log.push('\n');
        fail(
            format!(
                "the page types have {} problems, first: {}",
                found.len(),
                found[0]
            ),
            &log,
        )
    })?;
    let found = schema.check_site(&env.site);
    if let Some(first) = found.first() {
        let list = found
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        log.push_str(&list);
        log.push('\n');
        return Err(fail(
            format!("the pages have {} problems, first: {first}", found.len()),
            &log,
        ));
    }
    write_generated(&env.site, &schema).map_err(|e| fail(e, &log))?;

    // Dependencies, only when the lockfile changed since the last install.
    let lock = std::fs::read(env.site.join("package-lock.json"))
        .map_err(|_| fail("package-lock.json is missing".into(), &log))?;
    let lock_hash = hex(&Sha256::digest(&lock));
    // Each folder that is built has its own dependencies.
    let marker =
        env.data_dir
            .join("build")
            .join(if env.site == site::Site::at(&env.data_dir).path {
                "installed-lock.sha256".to_string()
            } else {
                format!(
                    "installed-lock-{}.sha256",
                    env.site.file_name().unwrap_or_default().to_string_lossy()
                )
            });
    let installed = std::fs::read_to_string(&marker).unwrap_or_default();
    if installed.trim() != lock_hash || !env.site.join("node_modules").is_dir() {
        run(
            &sandbox,
            &["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund"],
            &mut log,
        )
        .await
        .map_err(|e| fail(format!("installing the dependencies failed: {e}"), &log))?;
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, &lock_hash);
    }

    let dist = env.site.join("dist");
    let _ = std::fs::remove_dir_all(&dist);
    run(&sandbox, &["npm", "run", "build"], &mut log)
        .await
        .map_err(|e| fail(format!("the build failed: {e}"), &log))?;

    if !dist.join("index.html").is_file() {
        return Err(fail(
            "the build produced no home page (dist/index.html)".into(),
            &log,
        ));
    }
    for reserved in ["api", "_alogeno"] {
        if dist.join(reserved).exists() {
            return Err(fail(
                format!("the build produced /{reserved}/, which is reserved for alogeno"),
                &log,
            ));
        }
    }

    let releases = env.releases();
    let release = site::ulid();
    let target = releases.join(&release);
    let copied =
        copy_tree(&dist, &target).map_err(|e| fail(format!("copying the release: {e}"), &log))?;
    // The pictures of the site, from the archive.
    crate::media::publish(
        &env.site,
        &crate::media::Archive::at(&env.data_dir),
        &target,
    )
    .map_err(|e| fail(format!("copying the pictures: {e}"), &log))?;
    if copied.skipped > 0 {
        log.push_str(&format!(
            "\nalogeno: {} symbolic links left out of the release\n",
            copied.skipped
        ));
    }
    let commit = git::run(&env.site, &["rev-parse", "HEAD"]).unwrap_or_default();
    // The files alogeno generates in .alogeno/ do not count: the build
    // writes them itself, and commits leave them out.
    let dirty = !git::run(
        &env.site,
        &["status", "--porcelain", "--", ".", ":(exclude).alogeno"],
    )
    .unwrap_or_default()
    .trim()
    .is_empty();
    let manifest = serde_json::json!({
        "id": release,
        "commit": commit.trim(),
        "uncommitted_changes": dirty,
        "built_at_unix": now(),
        "files": copied.files,
        "bytes": copied.bytes,
    });
    std::fs::write(
        target.join(".manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .map_err(|e| fail(format!("writing the manifest: {e}"), &log))?;
    seal(&target);
    Ok((release, tail(&log)))
}

/// Runs one command in the sandbox, appending its output to `log`.
async fn run(sandbox: &Sandbox<'_>, command: &[&str], log: &mut String) -> Result<(), String> {
    let (program, args) = command.split_first().expect("a command");
    let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
    log.push_str(&format!("$ {}\n", command.join(" ")));
    let (std_command, _filter) = sandbox
        .command(program, &args)
        .map_err(|e| e.to_string())?
        .into_parts();
    let output = tokio::process::Command::from(std_command)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(())
    } else {
        Err(match output.status.code() {
            Some(code) => format!("exit code {code}"),
            None => "stopped by the system, possibly out of memory".into(),
        })
    }
}

#[derive(Default)]
struct Copied {
    files: u64,
    bytes: u64,
    skipped: u64,
}

/// Copies regular files and directories only. Symbolic links written by the
/// build could point anywhere on the server; they never reach a release.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<Copied> {
    let mut copied = Copied::default();
    copy_into(from, to, &mut copied)?;
    Ok(copied)
}

fn copy_into(from: &Path, to: &Path, copied: &mut Copied) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_into(&entry.path(), &target, copied)?;
        } else if kind.is_file() {
            copied.bytes += std::fs::copy(entry.path(), &target)?;
            copied.files += 1;
        } else {
            copied.skipped += 1;
        }
    }
    Ok(())
}

/// Makes a release read-only.
fn seal(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            seal(&path);
        } else {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444));
        }
    }
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555));
}

/// Points `releases/<name>` to `release`, atomically.
pub fn point(releases: &Path, name: &str, release: &str) -> std::io::Result<()> {
    let temporary = releases.join(format!(".{name}.new"));
    let _ = std::fs::remove_file(&temporary);
    std::os::unix::fs::symlink(release, &temporary)?;
    std::fs::rename(&temporary, releases.join(name))
}

/// Removes old releases, keeping the newest ones and any in use.
pub fn prune(releases: &Path) {
    let in_use: Vec<String> = ["preview", "live", "previous"]
        .iter()
        .filter_map(|name| {
            std::fs::read_link(releases.join(name))
                .ok()
                .and_then(|t| t.to_str().map(String::from))
        })
        .collect();
    let Ok(entries) = std::fs::read_dir(releases) else {
        return;
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|name| !name.starts_with('.'))
        .collect();
    ids.sort();
    let excess = ids.len().saturating_sub(KEEP);
    for id in ids.into_iter().take(excess) {
        if in_use.contains(&id) {
            continue;
        }
        let dir = releases.join(&id);
        unseal(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Makes the directories of a sealed release writable again, so that it can
/// be removed.
fn unseal(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for entry in walk(dir) {
        let _ = std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755));
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    let mut index = 0;
    while index < dirs.len() {
        if let Ok(entries) = std::fs::read_dir(&dirs[index]) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    dirs.push(entry.path());
                }
            }
        }
        index += 1;
    }
    dirs
}

fn tail(log: &str) -> String {
    if log.len() <= LOG_TAIL {
        return log.to_string();
    }
    let mut start = log.len() - LOG_TAIL;
    while !log.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &log[start..])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Seconds since the Unix epoch.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A builder that never builds anything, for tests of other modules.
#[cfg(test)]
pub fn idle(data_dir: &Path) -> Arc<Builder> {
    Builder::start(Environment {
        data_dir: data_dir.to_path_buf(),
        site: data_dir.join("site"),
        runtimes: Runtimes {
            node: PathBuf::from("/nonexistent/node"),
            claude: PathBuf::from("/nonexistent/claude"),
            codex: None,
            managed: false,
            codex_managed: false,
        },
        bwrap: None,
        alogeno: PathBuf::from("/nonexistent/alogeno"),
        proxy: PathBuf::from("/nonexistent/build.sock"),
        cgroup: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiting_returns_the_build_that_includes_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let builder = idle(dir.path());
        let first = builder.build_and_wait().await;
        assert_eq!(first.state, "failed");
        assert_eq!(first.covers, 1);
        assert!(first.error.is_some());
        assert_eq!(builder.build_and_wait().await.covers, 2);
    }

    #[test]
    fn copies_files_but_not_symbolic_links() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("dist");
        std::fs::create_dir_all(from.join("about")).unwrap();
        std::fs::write(from.join("index.html"), "home").unwrap();
        std::fs::write(from.join("about/index.html"), "about").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", from.join("leak.txt")).unwrap();
        let to = dir.path().join("release");
        let copied = copy_tree(&from, &to).unwrap();
        assert_eq!((copied.files, copied.skipped), (2, 1));
        assert!(!to.join("leak.txt").exists());
        assert_eq!(
            std::fs::read_to_string(to.join("about/index.html")).unwrap(),
            "about"
        );
    }

    #[test]
    fn points_and_prunes_releases() {
        let dir = tempfile::tempdir().unwrap();
        let releases = dir.path();
        for i in 0..(KEEP + 3) {
            let release = releases.join(format!("R{i:02}"));
            std::fs::create_dir_all(&release).unwrap();
            std::fs::write(release.join("index.html"), "x").unwrap();
            seal(&release);
        }
        point(releases, "preview", "R00").unwrap();
        assert_eq!(current_release(releases).as_deref(), Some("R00"));
        prune(releases);
        let mut left: Vec<String> = std::fs::read_dir(releases)
            .unwrap()
            .flatten()
            .filter(|e| e.file_type().unwrap().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP + 1, "{left:?}");
        assert!(left.contains(&"R00".to_string()), "the preview stays");
        assert!(!left.contains(&"R01".to_string()));
        for id in left {
            unseal(&releases.join(id));
        }
    }

    #[test]
    fn keeps_the_end_of_long_logs() {
        let log = "a".repeat(LOG_TAIL) + "end";
        let kept = tail(&log);
        assert!(kept.ends_with("end"));
        assert!(kept.len() <= LOG_TAIL + 4);
    }
}
