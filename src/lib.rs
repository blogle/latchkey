//! Latchkey gateway library.
//!
//! Two responsibilities live in this crate root:
//!
//! 1. The **module skeleton** (LATCH-2 / F02): every lane module is declared
//!    exactly once, here, so parallel tickets never have to edit the root.
//!    The domain types, async ports, and error taxonomy that those lanes
//!    implement are frozen in [`contracts`].
//! 2. The **LATCH-1 CLI surface**: honest behavior for the placeholder
//!    binary — `--help` and `--version` work, and every other invocation
//!    (in particular any attempt to serve) fails closed with a non-zero exit
//!    code.

// Root module declarations — frozen list, declared once (LATCH-2). All lane
// modules except `contracts` are empty files today; later tickets fill them
// in without touching this file.
pub mod catalog;
pub mod contracts;
pub mod downstream;
pub mod health;
pub mod kubernetes;
pub mod local_config;
pub mod reconcile;
pub mod router;
pub mod runtime;
pub mod search;
pub mod server;
pub mod telemetry;

use std::io::Write;

/// Exit code used when an invocation asks this binary to do more than report
/// help or version information (in particular, any attempt to serve).
pub const EXIT_NOT_IMPLEMENTED: u8 = 2;

/// The package version, as reported by `--version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Help text. States plainly that the gateway is not implemented.
pub const HELP: &str = "\
latchkey - bootstrap placeholder for the Latchkey gateway

USAGE:
    latchkey [--help | --version]

Status: the gateway is not implemented (bootstrap placeholder).
It supports exactly the options below; any attempt to serve (for example
`latchkey serve`) exits with a non-zero status instead of pretending to
work.

OPTIONS:
    -h, --help       Print this help text and exit
    -V, --version    Print the version and exit";

/// What the user asked the binary to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// `--help`, `-h`, or no arguments at all.
    Help,
    /// `--version` or `-V`.
    Version,
    /// Anything else: the gateway is not implemented.
    NotImplemented { requested: String },
}

/// Classify the given arguments (program name already stripped).
///
/// Only the first argument is consulted so that a serve request cannot be
/// smuggled past `--help`: `latchkey serve --help` still fails closed.
pub fn parse<I>(mut args: I) -> Invocation
where
    I: Iterator<Item = String>,
{
    match args.next() {
        None => Invocation::Help,
        Some(arg) if arg == "-h" || arg == "--help" => Invocation::Help,
        Some(arg) if arg == "-V" || arg == "--version" => Invocation::Version,
        Some(arg) => Invocation::NotImplemented { requested: arg },
    }
}

/// Run the CLI against the given writers and return the process exit code.
///
/// Help and version go to `out`; refusals go to `err`. A refusal always
/// returns [`EXIT_NOT_IMPLEMENTED`] (never 0) and names the unimplemented
/// gateway explicitly.
pub fn run<I, O, E>(args: I, out: &mut O, err: &mut E) -> u8
where
    I: IntoIterator<Item = String>,
    O: Write,
    E: Write,
{
    match parse(args.into_iter()) {
        Invocation::Help => {
            let _ = writeln!(out, "{HELP}");
            0
        }
        Invocation::Version => {
            let _ = writeln!(out, "latchkey {VERSION}");
            0
        }
        Invocation::NotImplemented { requested } => {
            let _ = writeln!(
                err,
                "error: latchkey refuses to serve: the gateway is not implemented"
            );
            let _ = writeln!(
                err,
                "hint: only --help and --version are supported (got `{requested}`)"
            );
            EXIT_NOT_IMPLEMENTED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the CLI with the given argv and capture stdout, stderr, and the
    /// exit code.
    fn run_args(args: &[&str]) -> (u8, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(args.iter().map(|s| (*s).to_string()), &mut out, &mut err);
        let out = String::from_utf8(out).expect("stdout is utf-8");
        let err = String::from_utf8(err).expect("stderr is utf-8");
        (code, out, err)
    }

    #[test]
    fn no_arguments_prints_help_and_succeeds() {
        let (code, out, err) = run_args(&[]);
        assert_eq!(code, 0);
        assert!(
            out.contains("USAGE:"),
            "help should print usage, got: {out}"
        );
        assert!(out.contains("not implemented"), "help must be honest");
        assert!(err.is_empty());
    }

    #[test]
    fn help_flag_prints_help_and_succeeds() {
        for flag in ["-h", "--help"] {
            let (code, out, err) = run_args(&[flag]);
            assert_eq!(code, 0, "{flag} should succeed");
            assert!(out.contains("USAGE:"), "{flag} should print usage");
            assert!(err.is_empty(), "{flag} should not write to stderr");
        }
    }

    #[test]
    fn version_flag_prints_package_version() {
        for flag in ["-V", "--version"] {
            let (code, out, err) = run_args(&[flag]);
            assert_eq!(code, 0, "{flag} should succeed");
            assert_eq!(out, format!("latchkey {VERSION}\n"));
            assert!(err.is_empty());
        }
    }

    #[test]
    fn serve_fails_closed_with_explicit_message() {
        let (code, out, err) = run_args(&["serve"]);
        assert_ne!(code, 0, "serve must never exit 0");
        assert_eq!(code, EXIT_NOT_IMPLEMENTED);
        assert!(out.is_empty(), "serve must not pretend to succeed");
        assert!(
            err.contains("the gateway is not implemented"),
            "error must state the gateway is not implemented, got: {err}"
        );
        assert!(err.contains("`serve`"), "error should echo the request");
    }

    #[test]
    fn other_serving_style_commands_also_fail_closed() {
        for cmd in ["gateway", "start", "--serve", "--port", "8080"] {
            let (code, out, err) = run_args(&[cmd]);
            assert_ne!(code, 0, "{cmd} must not exit 0");
            assert_eq!(code, EXIT_NOT_IMPLEMENTED, "{cmd}");
            assert!(out.is_empty(), "{cmd} must not write to stdout");
            assert!(
                err.contains("the gateway is not implemented"),
                "{cmd} should mention the unimplemented gateway"
            );
        }
    }

    #[test]
    fn help_cannot_mask_a_serve_request() {
        // Only the first argument is considered; `serve` is never ignored.
        let (code, _, _) = run_args(&["serve", "--help"]);
        assert_ne!(code, 0);
        assert_eq!(code, EXIT_NOT_IMPLEMENTED);

        // The mirror case: trailing junk after --help does not change verdicts
        // about the first argument either.
        let (code, out, _) = run_args(&["--help", "serve"]);
        assert_eq!(code, 0);
        assert!(out.contains("USAGE:"));
    }

    #[test]
    fn parse_classifies_arguments() {
        let parse = |args: &[&str]| parse(args.iter().map(|s| (*s).to_string()));
        assert_eq!(parse(&[]), Invocation::Help);
        assert_eq!(parse(&["-h"]), Invocation::Help);
        assert_eq!(parse(&["--help"]), Invocation::Help);
        assert_eq!(parse(&["-V"]), Invocation::Version);
        assert_eq!(parse(&["--version"]), Invocation::Version);
        assert_eq!(
            parse(&["serve"]),
            Invocation::NotImplemented {
                requested: "serve".to_string()
            }
        );
        assert_eq!(
            parse(&["--unexpected"]),
            Invocation::NotImplemented {
                requested: "--unexpected".to_string()
            }
        );
    }

    #[test]
    fn version_matches_the_manifest() {
        let (code, out, _) = run_args(&["--version"]);
        assert_eq!(code, 0);
        assert_eq!(out.trim_end(), format!("latchkey {VERSION}"));
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
