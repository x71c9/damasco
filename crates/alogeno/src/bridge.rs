//! Runs inside the sandbox, where there is no network: listens on
//! 127.0.0.1:3128, forwards every connection to the egress proxy's Unix
//! socket, and runs the actual command with the proxy variables set. Its exit
//! code is the command's.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use tokio::net::{TcpListener, UnixStream};

pub const PROXY_ADDRESS: &str = "127.0.0.1:3128";

pub async fn run(socket: &Path, command: &[OsString]) -> ExitCode {
    match bridge(socket, command).await {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("alogeno bridge: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn bridge(socket: &Path, command: &[OsString]) -> std::io::Result<i32> {
    let listener = TcpListener::bind(PROXY_ADDRESS).await?;
    let (program, args) = command
        .split_first()
        .ok_or_else(|| std::io::Error::other("no command"))?;
    let proxy = format!("http://{PROXY_ADDRESS}");
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .env("HTTPS_PROXY", &proxy)
        .env("https_proxy", &proxy)
        .env("HTTP_PROXY", &proxy)
        .env("http_proxy", &proxy)
        .env("NO_PROXY", "localhost,127.0.0.1")
        .env("no_proxy", "localhost,127.0.0.1")
        // Node's built-in fetch and http follow the variables only with this.
        .env("NODE_USE_ENV_PROXY", "1")
        .spawn()?;
    let socket = socket.to_path_buf();
    let forward = async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                continue;
            };
            let socket = socket.clone();
            tokio::spawn(async move {
                if let Ok(mut unix) = UnixStream::connect(&socket).await {
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
                }
            });
        }
    };
    tokio::select! {
        status = child.wait() => Ok(status?.code().unwrap_or(1)),
        () = forward => Ok(1),
    }
}
