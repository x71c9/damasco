//! axioma: self-hosted platform that builds and maintains a website through a
//! chat with a coding agent.

use std::net::SocketAddr;
use std::process::ExitCode;

use axum::{Router, routing::get};

/// Address used when `AXIOMA_LISTEN` is not set.
const DEFAULT_LISTEN: &str = "127.0.0.1:4100";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("axioma: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> std::io::Result<()> {
    let addr = listen_addr()?;
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|err| {
        std::io::Error::new(err.kind(), format!("cannot listen on {addr}: {err}"))
    })?;
    eprintln!("axioma: listening on http://{}", listener.local_addr()?);
    axum::serve(listener, router())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    eprintln!("axioma: stopped");
    Ok(())
}

/// Reads the listen address from `AXIOMA_LISTEN`, falling back to the default.
fn listen_addr() -> std::io::Result<SocketAddr> {
    let value = std::env::var("AXIOMA_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());
    value.parse().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("AXIOMA_LISTEN={value}: {err}"),
        )
    })
}

fn router() -> Router {
    Router::new().route("/_axioma/health", get(health))
}

async fn health() -> &'static str {
    "ok"
}

/// Resolves on Ctrl-C or SIGTERM, so that systemd stops the service cleanly.
async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    async fn status_of(path: &str) -> StatusCode {
        let request = Request::get(path).body(Body::empty()).unwrap();
        router().oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn health_is_ok() {
        assert_eq!(status_of("/_axioma/health").await, StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_path_is_not_found() {
        assert_eq!(status_of("/missing").await, StatusCode::NOT_FOUND);
    }
}
