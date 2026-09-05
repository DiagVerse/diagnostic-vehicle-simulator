//! `dvsim` — the composition root binary.
//!
//! It wires the layers together: initialise logging, load plugins from the drop-in
//! directory, then serve the HTTP API. This is the only place that knows about all layers at
//! once; everything else depends inward.
//!
//! Usage:
//!   dvsim serve [--addr 127.0.0.1:8080] [--plugins <dir>]
//!
//! With no arguments it defaults to `serve` on 127.0.0.1:8080 reading `plugins.d/`.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use api::AppState;
use application::{plugin_host::default_plugin_dir, PluginHost};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cmd = Cli::parse(std::env::args().skip(1));
    match cmd {
        Cli::Serve { addr, plugins } => serve(addr, plugins).await,
        Cli::Help => {
            print_help();
            Ok(())
        }
    }
}

/// Initialise structured logging. Honour `RUST_LOG`, defaulting to `info`.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

async fn serve(addr: SocketAddr, plugins_dir: PathBuf) -> anyhow::Result<()> {
    tracing::info!(%addr, plugins = %plugins_dir.display(), "starting dvsim engine");

    let host = PluginHost::load_from_dir(&plugins_dir);
    let state = Arc::new(AppState {
        plugins: host.infos().to_vec(),
    });

    api::serve(addr, state)
        .await
        .with_context(|| format!("server error on {addr}"))?;
    Ok(())
}

/// Minimal hand-rolled argument parsing — no external CLI dependency needed yet.
enum Cli {
    Serve { addr: SocketAddr, plugins: PathBuf },
    Help,
}

impl Cli {
    fn parse(args: impl Iterator<Item = String>) -> Self {
        let mut addr: SocketAddr = "127.0.0.1:8080".parse().expect("valid default addr");
        let mut plugins = default_plugin_dir();
        let mut subcommand: Option<String> = None;

        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" | "help" => return Cli::Help,
                "--addr" => {
                    if let Some(v) = args.next() {
                        if let Ok(parsed) = v.parse() {
                            addr = parsed;
                        }
                    }
                }
                "--plugins" => {
                    if let Some(v) = args.next() {
                        plugins = PathBuf::from(v);
                    }
                }
                other if subcommand.is_none() => subcommand = Some(other.to_string()),
                _ => {}
            }
        }

        match subcommand.as_deref() {
            None | Some("serve") => Cli::Serve { addr, plugins },
            _ => Cli::Help,
        }
    }
}

fn print_help() {
    println!(
        "dvsim — Diagnostic Vehicle Simulator engine\n\n\
         USAGE:\n    dvsim serve [--addr <ip:port>] [--plugins <dir>]\n\n\
         OPTIONS:\n\
         \x20   --addr <ip:port>   Address to listen on (default 127.0.0.1:8080)\n\
         \x20   --plugins <dir>    Plugin drop-in directory (default plugins.d)\n\
         \x20   -h, --help         Show this help"
    );
}
