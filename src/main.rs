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
    let options = parse_serve_args(args)?;
    latchkey::runtime::Runtime::standalone(options.config, options.listen, options.allowed_hosts)
        .await
}

struct ServeOptions {
    config: PathBuf,
    listen: Option<SocketAddr>,
    allowed_hosts: Vec<String>,
}

fn parse_serve_args(
    args: &[String],
) -> Result<ServeOptions, Box<dyn std::error::Error + Send + Sync>> {
    let mut mode = None;
    let mut config = None;
    let mut listen = None;
    let mut allowed_hosts = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let value = &args[index];
        let target = match value.as_str() {
            "--mode" => &mut mode,
            "--config" => &mut config,
            "--listen" => &mut listen,
            "--allowed-host" => {
                index += 1;
                let argument = args
                    .get(index)
                    .ok_or_else(|| format!("missing value for `{value}`"))?;
                if argument.starts_with("--") || argument.is_empty() {
                    return Err(format!("missing value for `{value}`").into());
                }
                allowed_hosts.push(argument.clone());
                index += 1;
                continue;
            }
            _ => return Err(format!("unknown serve option `{value}`").into()),
        };
        index += 1;
        let argument = args
            .get(index)
            .ok_or_else(|| format!("missing value for `{value}`"))?;
        if argument.starts_with("--") || argument.is_empty() {
            return Err(format!("missing value for `{value}`").into());
        }
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
    Ok(ServeOptions {
        config,
        listen,
        allowed_hosts,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_serve_args;

    fn parse(
        args: &[&str],
    ) -> Result<super::ServeOptions, Box<dyn std::error::Error + Send + Sync>> {
        parse_serve_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_repeatable_allowed_hosts() {
        let parsed = parse(&[
            "--mode",
            "standalone",
            "--config",
            "config.toml",
            "--listen",
            "0.0.0.0:8080",
            "--allowed-host",
            "latchkey.nexus.svc.cluster.local",
            "--allowed-host",
            "latchkey.example.test",
        ])
        .expect("valid serve arguments");
        assert_eq!(
            parsed.allowed_hosts,
            ["latchkey.nexus.svc.cluster.local", "latchkey.example.test"]
        );
    }

    #[test]
    fn rejects_missing_or_malformed_allowed_host_values() {
        for args in [
            &[
                "--mode",
                "standalone",
                "--config",
                "config.toml",
                "--allowed-host",
            ][..],
            &[
                "--mode",
                "standalone",
                "--config",
                "config.toml",
                "--allowed-host",
                "--listen",
                "0.0.0.0:8080",
            ][..],
        ] {
            assert!(parse(args).is_err(), "expected parse failure for {args:?}");
        }
    }
}
