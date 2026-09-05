//! `dvsim` — the composition root binary.
//!
//! It wires the layers together: initialise logging, load plugins from the drop-in
//! directory, then serve the HTTP API. This is the only place that knows about all layers at
//! once; everything else depends inward.
//!
//! Usage:
//!   dvsim serve       [--addr 127.0.0.1:8080] [--plugins <dir>]
//!   dvsim demo        [--plugins <dir>]
//!   dvsim reconstruct <canlog-file>
//!
//! With no arguments it defaults to `serve` on 127.0.0.1:8080 reading `plugins.d/`.

mod demo;
mod reconstruct_cmd;

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
        Cli::Demo { plugins } => demo::Run(&plugins),
        Cli::Reconstruct { path } => reconstruct_cmd::Run(&path),
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

    // Resolve the UDS protocol handler once at startup; the diagnostics endpoints report it as
    // unavailable (rather than failing) if the plugin was not present.
    let protocol = host.FindProtocol("uds");
    if protocol.is_none() {
        tracing::warn!(
            "no 'uds' protocol plugin loaded; /ecu/* endpoints will report it unavailable"
        );
    }

    let state = Arc::new(AppState {
        plugins: host.infos().to_vec(),
        protocol,
        ecu: std::sync::Mutex::new(ecu::VirtualEcu::New(ecu::sample::BuildEngineEcu())),
    });

    api::serve(addr, state)
        .await
        .with_context(|| format!("server error on {addr}"))?;
    Ok(())
}

/// Minimal hand-rolled argument parsing — no external CLI dependency needed yet.
enum Cli {
    Serve { addr: SocketAddr, plugins: PathBuf },
    Demo { plugins: PathBuf },
    Reconstruct { path: PathBuf },
    Help,
}

impl Cli {
    fn parse(args: impl Iterator<Item = String>) -> Self {
        let mut addr: SocketAddr = "127.0.0.1:8080".parse().expect("valid default addr");
        let mut plugins = default_plugin_dir();
        // Positional arguments in order: [0] = subcommand, [1] = subcommand argument.
        let mut positional: Vec<String> = Vec::new();

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
                other => positional.push(other.to_string()),
            }
        }

        match positional.first().map(String::as_str) {
            None | Some("serve") => Cli::Serve { addr, plugins },
            Some("demo") => Cli::Demo { plugins },
            Some("reconstruct") => match positional.get(1) {
                Some(path) => Cli::Reconstruct {
                    path: PathBuf::from(path),
                },
                None => Cli::Help,
            },
            _ => Cli::Help,
        }
    }
}

fn print_help() {
    println!(
        "dvsim — Diagnostic Vehicle Simulator engine\n\n\
         USAGE:\n\
         \x20   dvsim serve       [--addr <ip:port>] [--plugins <dir>]\n\
         \x20   dvsim demo        [--plugins <dir>]\n\
         \x20   dvsim reconstruct <canlog-file>\n\n\
         OPTIONS:\n\
         \x20   --addr <ip:port>   Address to listen on (default 127.0.0.1:8080)\n\
         \x20   --plugins <dir>    Plugin drop-in directory (default plugins.d)\n\
         \x20   -h, --help         Show this help"
    );
}
