//! The `meridian-report-server` command.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use meridian_report_server::store::Store;
use meridian_report_server::{router, App};

#[derive(Parser)]
#[command(
    name = "meridian-report-server",
    version,
    about = "Receives the events of Meridian runs and shows them: runs per project, each run's report, every report format"
)]
struct Cli {
    /// The address to listen on. Plain HTTP: keep it on this machine, or
    /// put it behind what terminates TLS for you
    #[arg(
        long,
        value_name = "ADDR",
        env = "MERIDIAN_SERVER_LISTEN",
        default_value = "127.0.0.1:8717"
    )]
    listen: SocketAddr,
    /// Where the events are kept
    #[arg(
        long,
        value_name = "DIR",
        env = "MERIDIAN_SERVER_DATA",
        default_value = "meridian-reports"
    )]
    data: PathBuf,
    /// A file holding the token a sender must present to add events. The
    /// environment variable MERIDIAN_SERVER_TOKEN is read when this is not
    /// given. One of the two is required
    #[arg(long, value_name = "FILE")]
    token_file: Option<PathBuf>,
    /// A file holding a token a reader must present. Without it, and
    /// without MERIDIAN_SERVER_READ_TOKEN, anyone who can reach the server
    /// can read it
    #[arg(long, value_name = "FILE")]
    read_token_file: Option<PathBuf>,
    /// How many runs each project keeps; the oldest are removed
    #[arg(long, value_name = "N", default_value_t = 1000)]
    keep_runs: usize,
}

/// A token from a file, or else from the environment. Never from an
/// argument: arguments are visible to every user of the machine.
fn token(file: Option<&PathBuf>, variable: &str) -> Result<Option<String>, String> {
    let raw = match file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|e| format!("reading the token file {}: {e}", path.display()))?,
        ),
        None => std::env::var(variable).ok(),
    };
    match raw.map(|raw| raw.trim().to_owned()) {
        None => Ok(None),
        Some(token) if token.len() < 16 => Err(format!(
            "the token from {} is shorter than 16 characters; use a long random one",
            file.map_or_else(|| variable.to_owned(), |p| p.display().to_string())
        )),
        Some(token) => Ok(Some(token)),
    }
}

async fn serve(cli: Cli) -> Result<(), String> {
    let write_token = token(cli.token_file.as_ref(), "MERIDIAN_SERVER_TOKEN")?
        .ok_or("a write token is required: set MERIDIAN_SERVER_TOKEN, or give --token-file FILE")?;
    let read_token = token(cli.read_token_file.as_ref(), "MERIDIAN_SERVER_READ_TOKEN")?;
    std::fs::create_dir_all(&cli.data)
        .map_err(|e| format!("the data directory {}: {e}", cli.data.display()))?;
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .map_err(|e| format!("listening on {}: {e}", cli.listen))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    // The first line of output is the address, so a script that asked for
    // port 0 can read which port it got.
    println!("listening on http://{address}");
    if read_token.is_none() {
        eprintln!(
            "note: no read token is set, so anyone who can reach {address} can read the runs"
        );
    }
    if !address.ip().is_loopback() {
        eprintln!("note: this is plain HTTP; put it behind what terminates TLS for you");
    }
    let app = Arc::new(App {
        store: Store::new(cli.data, cli.keep_runs),
        write_token,
        read_token,
    });
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}
