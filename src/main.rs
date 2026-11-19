use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use binlookup::config::Config;
use binlookup::routes::app_router;
use binlookup::store::BinStore;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [flag, input, output] if flag == "--compile" => {
            return compile_snapshot(input, output);
        }
        _ => {
            error!("usage: binlookup [--compile <input.csv> <output.bin>]");
            return ExitCode::FAILURE;
        }
    }

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            error!(error = %err, "invalid configuration");
            return ExitCode::FAILURE;
        }
    };

    let started = Instant::now();
    let store = match BinStore::from_path(&config.data_path) {
        Ok(store) => Arc::new(store),
        Err(err) => {
            error!(path = %config.data_path, error = %err, "failed to load BIN dataset");
            return ExitCode::FAILURE;
        }
    };
    info!(
        path = %config.data_path,
        records = store.len(),
        memory_kb = store.memory_bytes() / 1024,
        elapsed_ms = started.elapsed().as_millis(),
        "loaded BIN dataset"
    );

    let listener = match TcpListener::bind((config.host.as_str(), config.port)).await {
        Ok(listener) => listener,
        Err(err) => {
            error!(host = %config.host, port = config.port, error = %err, "failed to bind TCP listener");
            return ExitCode::FAILURE;
        }
    };

    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| format!("{}:{}", config.host, config.port));
    info!(addr = %addr, "listening for requests");

    let app = app_router(store);
    if let Err(err) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        error!(error = %err, "server exited with error");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn compile_snapshot(input: &str, output: &str) -> ExitCode {
    let started = Instant::now();
    let store = match BinStore::from_path(input) {
        Ok(store) => store,
        Err(err) => {
            error!(input = %input, error = %err, "failed to load BIN dataset");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = store.save_snapshot(output) {
        error!(output = %output, error = %err, "failed to write BIN snapshot");
        return ExitCode::FAILURE;
    }
    info!(
        input = %input,
        output = %output,
        records = store.len(),
        memory_kb = store.memory_bytes() / 1024,
        elapsed_ms = started.elapsed().as_millis(),
        "compiled BIN snapshot"
    );
    ExitCode::SUCCESS
}

async fn shutdown_signal() {
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => info!("received Ctrl+C, starting graceful shutdown"),
            Err(err) => {
                error!(error = %err, "failed to listen for Ctrl+C signal");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
                info!("received SIGTERM, starting graceful shutdown");
            }
            Err(err) => {
                error!(error = %err, "failed to install SIGTERM signal handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
