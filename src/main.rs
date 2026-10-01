//! Latchkey bootstrap binary.
//!
//! Thin wrapper: collect argv, delegate to [`latchkey::run`], and exit with
//! the resulting code. See the library docs for the honest-surface contract.

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut out = io::stdout().lock();
    let mut err = io::stderr().lock();
    let code = latchkey::run(std::env::args().skip(1), &mut out, &mut err);
    let _ = out.flush();
    let _ = err.flush();
    ExitCode::from(code)
}
