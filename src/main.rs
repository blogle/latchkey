//! Latchkey bootstrap binary.
//!
//! Thin wrapper: collect argv, delegate to [`latchkey::run`], and exit with
//! the resulting code. See the library docs for the honest-surface contract.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    latchkey::runtime::install_crypto_provider();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "serve") {
        return match serve_args(&args[1..]).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        };
    }
    let mut out = io::stdout().lock();
    let mut err = io::stderr().lock();
    let code = latchkey::run(args, &mut out, &mut err);
    let _ = out.flush();
    let _ = err.flush();
    ExitCode::from(code)
}

async fn serve_args(args: &[String]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut mode = None;
    let mut config = None;
    let mut listen = None;
    let mut index = 0;
    while index < args.len() {
        let value = &args[index];
        let target = match value.as_str() {
            "--mode" => &mut mode,
            "--config" => &mut config,
            "--listen" => &mut listen,
            _ => return Err(format!("unknown serve option `{value}`").into()),
        };
        index += 1;
        let argument = args
            .get(index)
            .ok_or_else(|| format!("missing value for `{value}`"))?;
        *target = Some(argument.clone());
        index += 1;
    }
    if mode.as_deref() != Some("standalone") {
        return Err("serve requires --mode standalone".into());
    }
    let config = PathBuf::from(config.ok_or("serve requires --config PATH")?);
    let listen = listen
        .map(|value| value.parse::<SocketAddr>())
        .transpose()?;
    latchkey::runtime::Runtime::standalone(config, listen).await
}
