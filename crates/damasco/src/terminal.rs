//! A program on a pseudo-terminal, for the terminal page of the panel.
//!
//! damasco opens the terminal pair, gives the program the secondary side as
//! its standard input, output and controlling terminal, and keeps the
//! primary side: what the program writes goes to every browser attached,
//! what a browser types goes to the program. The last output is kept, so a
//! browser that attaches again (after a reload) sees the screen.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};

/// Output kept for a browser that attaches again.
const HISTORY: usize = 256 * 1024;

pub struct Terminal {
    primary: std::fs::File,
    pid: libc::pid_t,
    output: broadcast::Sender<Vec<u8>>,
    history: Mutex<VecDeque<u8>>,
    exited: watch::Sender<bool>,
}

impl Terminal {
    /// Starts `command` on a new terminal of `columns` × `rows`. `filter`
    /// is the descriptor the command needs until it starts.
    pub fn spawn(
        mut command: std::process::Command,
        filter: OwnedFd,
        columns: u16,
        rows: u16,
    ) -> std::io::Result<Arc<Self>> {
        let (primary, secondary) = open_pair(columns, rows)?;
        command
            .stdin(Stdio::from(secondary.try_clone()?))
            .stdout(Stdio::from(secondary.try_clone()?))
            .stderr(Stdio::from(secondary));
        // SAFETY: setsid and ioctl are async-signal-safe; standard input is
        // the terminal's secondary side by now.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The cast matches the request type of every C library.
                #[allow(clippy::unnecessary_cast)]
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        drop(filter);
        // The command holds the secondary side now; it is closed here, so
        // reading the primary side ends when the command does.
        drop(command);
        let (output, _) = broadcast::channel(256);
        let (exited, _) = watch::channel(false);
        let terminal = Arc::new(Self {
            primary: std::fs::File::from(primary),
            pid: child.id() as libc::pid_t,
            output,
            history: Mutex::new(VecDeque::new()),
            exited,
        });
        let reader = terminal.clone();
        let mut source = terminal.primary.try_clone()?;
        std::thread::spawn(move || {
            let mut buffer = [0u8; 16 * 1024];
            while let Ok(read) = source.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                let chunk = buffer[..read].to_vec();
                {
                    let mut history = reader.history.lock().unwrap_or_else(|e| e.into_inner());
                    history.extend(&chunk);
                    let excess = history.len().saturating_sub(HISTORY);
                    history.drain(..excess);
                }
                let _ = reader.output.send(chunk);
            }
        });
        let waiter = terminal.clone();
        std::thread::spawn(move || {
            let _ = child.wait();
            waiter.exited.send_replace(true);
        });
        Ok(terminal)
    }

    /// The output so far, and the output to come.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        // Subscribed under the lock: no output falls between the two.
        let receiver = self.output.subscribe();
        (history.iter().copied().collect(), receiver)
    }

    /// Passes typed input to the program.
    pub fn write(&self, input: &[u8]) -> std::io::Result<()> {
        (&self.primary).write_all(input)
    }

    pub fn resize(&self, columns: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows.max(1),
            ws_col: columns.max(1),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the descriptor is the primary side, open for our lifetime.
        #[allow(clippy::unnecessary_cast)]
        unsafe {
            libc::ioctl(self.primary.as_raw_fd(), libc::TIOCSWINSZ as _, &size);
        }
    }

    /// Ends the program: the sandbox goes with it.
    pub fn kill(&self) {
        if !self.has_exited() {
            // SAFETY: plain signal to our own child.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
        }
    }

    pub fn has_exited(&self) -> bool {
        *self.exited.borrow()
    }

    /// Completes when the program has ended.
    pub async fn exited(&self) {
        let mut exited = self.exited.subscribe();
        let _ = exited.wait_for(|exited| *exited).await;
    }
}

/// A new terminal pair: the primary side for damasco, the secondary side for
/// the program, both closed on exec.
fn open_pair(columns: u16, rows: u16) -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut primary: libc::c_int = -1;
    let mut secondary: libc::c_int = -1;
    let size = libc::winsize {
        ws_row: rows.max(1),
        ws_col: columns.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes two descriptors it opened; the name buffer is
    // not used.
    let result = unsafe {
        libc::openpty(
            &mut primary,
            &mut secondary,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just opened and belong to nobody else.
    let (primary, secondary) = unsafe {
        (
            OwnedFd::from_raw_fd(primary),
            OwnedFd::from_raw_fd(secondary),
        )
    };
    for fd in [&primary, &secondary] {
        // SAFETY: setting a flag on a descriptor we own.
        unsafe {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    Ok((primary, secondary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn output_until(receiver: &mut broadcast::Receiver<Vec<u8>>, text: &str) -> String {
        let mut seen = String::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !seen.contains(text) {
                match receiver.recv().await {
                    Ok(chunk) => seen.push_str(&String::from_utf8_lossy(&chunk)),
                    Err(_) => break,
                }
            }
        })
        .await
        .expect("output in time");
        seen
    }

    #[tokio::test]
    async fn runs_a_program_on_a_terminal() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "stty size; read line; echo got:$line"]);
        let filter = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
        let terminal = Terminal::spawn(command, filter, 100, 30).unwrap();
        let (_, mut output) = terminal.attach();
        let first = output_until(&mut output, "30 100").await;
        assert!(first.contains("30 100"), "{first:?}");
        terminal.write(b"hello\n").unwrap();
        let then = output_until(&mut output, "got:hello").await;
        assert!(then.contains("got:hello"));
        tokio::time::timeout(Duration::from_secs(5), terminal.exited())
            .await
            .unwrap();
        // A browser attaching later sees what happened.
        let (history, _) = terminal.attach();
        assert!(String::from_utf8_lossy(&history).contains("got:hello"));
    }

    #[tokio::test]
    async fn resizes_and_kills() {
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            "trap 'stty size' WINCH; while true; do sleep 0.05; done",
        ]);
        let filter = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
        let terminal = Terminal::spawn(command, filter, 80, 24).unwrap();
        let (_, mut output) = terminal.attach();
        tokio::time::sleep(Duration::from_millis(200)).await;
        terminal.resize(120, 40);
        output_until(&mut output, "40 120").await;
        terminal.kill();
        tokio::time::timeout(Duration::from_secs(5), terminal.exited())
            .await
            .unwrap();
        assert!(terminal.has_exited());
    }
}
