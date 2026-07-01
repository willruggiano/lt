//! User-facing command output.
//!
//! Commands write their results through a writer (`writeln!(out, ...)`) rather
//! than the `println!` family. This keeps `clippy::print_stdout` free to flag
//! stray debug prints, lets stdout be redirected (e.g. captured in tests), and
//! gives all command output a single home to evolve (paging, colour, ...).
//!
//! Diagnostics belong in `tracing`, not here; interactive prompts write to
//! stderr so stdout stays a clean, pipeable result stream.

use std::io::{self, Write};

/// The process stdout sink for command output.
///
/// Construct once near the entrypoint and thread `&mut Output` (as
/// `&mut dyn Write`) down to the commands.
pub struct Output(io::Stdout);

impl Output {
    /// Output writing to the process stdout.
    pub fn stdout() -> Self {
        Self(io::stdout())
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
