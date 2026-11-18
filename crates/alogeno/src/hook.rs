//! The command the CLI runs for its hooks, inside the agent sandbox.
//!
//! Claude Code passes the hook's event as JSON on standard input and reads
//! the decision from standard output. This command forwards the event to
//! alogeno through a Unix socket mounted in the sandbox and prints alogeno's
//! answer. When alogeno cannot be reached it prints nothing: in the CLI's
//! structured mode a request without a decision is refused.

use std::path::Path;
use std::process::ExitCode;

use std::io::{Read, Write};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Largest event or answer passed through.
pub const LIMIT: u64 = 1 << 20;

pub async fn run(socket: &Path) -> ExitCode {
    let mut event = Vec::new();
    if let Err(err) = std::io::stdin().take(LIMIT).read_to_end(&mut event) {
        eprintln!("alogeno hook: cannot read the event: {err}");
        return ExitCode::SUCCESS;
    }
    match exchange(socket, &event).await {
        Ok(answer) => {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&answer);
            let _ = stdout.flush();
        }
        Err(err) => eprintln!("alogeno hook: alogeno did not answer: {err}"),
    }
    ExitCode::SUCCESS
}

/// Sends the event and reads the answer until alogeno closes the connection.
pub async fn exchange(socket: &Path, event: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut stream = UnixStream::connect(socket).await?;
    stream.write_all(event).await?;
    stream.shutdown().await?;
    let mut answer = Vec::new();
    stream.take(LIMIT).read_to_end(&mut answer).await?;
    Ok(answer)
}
