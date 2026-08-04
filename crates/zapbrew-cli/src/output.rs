//! Production terminal reporter and the process exit contract.
//!
//! [`TerminalReporter`] is the single production [`Reporter`]: it owns the
//! `==>` / `Warning:` / `Error:` decoration, per-stream color gating, quiet
//! headline suppression, and verbose title behavior. [`report_error`],
//! [`exit_code`], [`success`], and [`SIGINT_EXIT_CODE`] carry the exit contract
//! that later Task 7 slices drive from `main`.

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;

use owo_colors::OwoColorize;
use zapbrew_ops::{OpError, Reporter};
use zapbrew_prefix::Env;

/// Conventional exit code for a SIGINT-interrupted run (`128 + SIGINT`).
pub const SIGINT_EXIT_CODE: u8 = 130;

/// Exit code for a successful run.
pub fn success() -> ExitCode {
    ExitCode::SUCCESS
}

/// Process exit code for a failed run. Every [`OpError`] variant maps to the
/// single failure code; the approved plan defines no per-variant codes.
pub fn exit_code(error: &OpError) -> ExitCode {
    ExitCode::from(failure_status(error))
}

/// Byte-testable seam behind [`exit_code`]: the numeric status shared by every
/// [`OpError`] variant.
fn failure_status(_error: &OpError) -> u8 {
    1
}

/// Render an operation error through `onoe` exactly once. The lower crates keep
/// their `Display` prefix-free, so the reporter is the sole owner of the
/// `Error:` label; this never double-prefixes.
pub fn report_error(reporter: &dyn Reporter, error: &OpError) {
    reporter.onoe(&error.to_string());
}

/// One output stream as a byte sink. Production writes the process stream; tests
/// capture into a shared buffer.
trait Sink: Send + Sync {
    fn emit(&self, bytes: &[u8]);
}

/// A process standard stream (stdout or stderr).
struct StdStream {
    stderr: bool,
}

impl Sink for StdStream {
    fn emit(&self, bytes: &[u8]) {
        if self.stderr {
            let mut stream = io::stderr().lock();
            let _ = stream.write_all(bytes);
            let _ = stream.flush();
        } else {
            let mut stream = io::stdout().lock();
            let _ = stream.write_all(bytes);
            let _ = stream.flush();
        }
    }
}

/// The single production [`Reporter`]. Owns per-stream sinks plus the color,
/// verbosity, width, and hint-program state resolved from the environment.
pub struct TerminalReporter {
    out: Arc<dyn Sink>,
    err: Arc<dyn Sink>,
    out_color: bool,
    err_color: bool,
    quiet: bool,
    verbose: bool,
    out_width: Option<usize>,
    program: &'static str,
}

impl TerminalReporter {
    /// Build the production reporter from the resolved environment, the clap
    /// quiet/verbose globals, and the argv0-derived hint program.
    ///
    /// Color follows brew `Tty.color?` independently per stream; the title width
    /// budget is captured only for an interactive, non-verbose stdout.
    pub fn from_env(env: &Env, quiet: bool, verbose: bool, program: &'static str) -> Self {
        let out_tty = io::stdout().is_terminal();
        let err_tty = io::stderr().is_terminal();
        Self {
            out: Arc::new(StdStream { stderr: false }),
            err: Arc::new(StdStream { stderr: true }),
            out_color: colorize(env, out_tty),
            err_color: colorize(env, err_tty),
            quiet,
            verbose,
            out_width: (out_tty && !verbose).then(terminal_width).flatten(),
            program,
        }
    }
}

impl Reporter for TerminalReporter {
    fn ohai(&self, message: &str) {
        if self.quiet {
            return;
        }
        self.out
            .emit(headline(message, Arrow::Blue, self.out_color, self.out_width).as_bytes());
    }

    fn oh1(&self, message: &str) {
        if self.quiet {
            return;
        }
        self.out
            .emit(headline(message, Arrow::Green, self.out_color, self.out_width).as_bytes());
    }

    fn opoo(&self, message: &str) {
        self.err
            .emit(warning_line(message, self.err_color).as_bytes());
    }

    fn onoe(&self, message: &str) {
        self.err
            .emit(error_line(message, self.err_color).as_bytes());
    }

    fn print(&self, message: &str) {
        self.out.emit(format!("{message}\n").as_bytes());
    }

    fn eprint(&self, message: &str) {
        self.err.emit(format!("{message}\n").as_bytes());
    }

    fn hint_program(&self) -> &str {
        self.program
    }

    fn is_quiet(&self) -> bool {
        self.quiet
    }

    fn is_verbose(&self) -> bool {
        self.verbose
    }
}

/// Per-stream color decision, matching brew `Tty.color?`: never when no-color,
/// otherwise forced-on or when the stream is a terminal.
fn colorize(env: &Env, is_tty: bool) -> bool {
    !env.no_color && (env.color || is_tty)
}

fn terminal_width() -> Option<usize> {
    terminal_size::terminal_size().map(|(width, _)| usize::from(width.0))
}

/// Arrow color distinguishing the two headline channels.
#[derive(Clone, Copy)]
enum Arrow {
    Blue,
    Green,
}

/// `==> {title}` headline plus a trailing newline. The colored form paints only
/// the arrow and bolds the title; the plain form is pure ASCII. A known width
/// truncates the title (TTY, non-verbose only).
fn headline(title: &str, arrow: Arrow, color: bool, width: Option<usize>) -> String {
    let title = truncate(title, width);
    if color {
        let arrow = match arrow {
            Arrow::Blue => format!("{}", "==>".blue()),
            Arrow::Green => format!("{}", "==>".green()),
        };
        format!("{arrow} {}\n", title.bold())
    } else {
        format!("==> {title}\n")
    }
}

/// Truncate a headline title to fit `width`, replacing the tail with an ellipsis
/// when it overflows. The `==> ` prefix occupies four columns, so the title
/// budget is `width - 4`; the ellipsis consumes the final column of that budget
/// so the rendered line never exceeds `width`. Unknown or tiny widths never
/// truncate.
fn truncate(title: &str, width: Option<usize>) -> String {
    match width {
        Some(width) if width > 4 && title.chars().count() > width - 4 => {
            let budget = width - 4;
            let head: String = title.chars().take(budget - 1).collect();
            format!("{head}\u{2026}")
        }
        _ => title.to_owned(),
    }
}

/// `Warning: {message}` on stderr plus a trailing newline. Colored form paints
/// only the label; a multi-line message keeps a single leading label.
fn warning_line(message: &str, color: bool) -> String {
    if color {
        format!("{} {message}\n", "Warning:".yellow())
    } else {
        format!("Warning: {message}\n")
    }
}

/// `Error: {message}` on stderr plus a trailing newline. Colored form paints
/// only the label; a multi-line message keeps a single leading label.
fn error_line(message: &str, color: bool) -> String {
    if color {
        format!("{} {message}\n", "Error:".red())
    } else {
        format!("Error: {message}\n")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const RESET: &str = "\u{1b}[39m";
    const BOLD: &str = "\u{1b}[1m";
    const UNBOLD: &str = "\u{1b}[0m";

    /// In-memory sink shared between a reporter and the test that inspects it.
    #[derive(Default)]
    struct Buffer(Mutex<Vec<u8>>);

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("buffer lock").clone()).expect("utf8 output")
        }
    }

    impl Sink for Buffer {
        fn emit(&self, bytes: &[u8]) {
            self.0.lock().expect("buffer lock").extend_from_slice(bytes);
        }
    }

    /// Reporter field overrides for a test harness; unspecified fields default.
    #[derive(Default)]
    struct Setup {
        out_color: bool,
        err_color: bool,
        quiet: bool,
        verbose: bool,
        out_width: Option<usize>,
        program: Option<&'static str>,
    }

    fn harness(setup: Setup) -> (TerminalReporter, Arc<Buffer>, Arc<Buffer>) {
        let out = Arc::new(Buffer::default());
        let err = Arc::new(Buffer::default());
        let reporter = TerminalReporter {
            out: out.clone(),
            err: err.clone(),
            out_color: setup.out_color,
            err_color: setup.err_color,
            quiet: setup.quiet,
            verbose: setup.verbose,
            out_width: setup.out_width,
            program: setup.program.unwrap_or("zapbrew"),
        };
        (reporter, out, err)
    }

    #[test]
    fn ohai_plain_and_colored_bytes() {
        assert_eq!(headline("Title", Arrow::Blue, false, None), "==> Title\n");
        assert_eq!(
            headline("Title", Arrow::Blue, true, None),
            format!("\u{1b}[34m==>{RESET} {BOLD}Title{UNBOLD}\n")
        );
    }

    #[test]
    fn oh1_plain_and_colored_bytes() {
        assert_eq!(headline("Title", Arrow::Green, false, None), "==> Title\n");
        assert_eq!(
            headline("Title", Arrow::Green, true, None),
            format!("\u{1b}[32m==>{RESET} {BOLD}Title{UNBOLD}\n")
        );
    }

    #[test]
    fn warning_plain_and_colored_bytes() {
        assert_eq!(warning_line("careful", false), "Warning: careful\n");
        assert_eq!(
            warning_line("careful", true),
            format!("\u{1b}[33mWarning:{RESET} careful\n")
        );
    }

    #[test]
    fn error_plain_and_colored_bytes() {
        assert_eq!(error_line("boom", false), "Error: boom\n");
        assert_eq!(
            error_line("boom", true),
            format!("\u{1b}[31mError:{RESET} boom\n")
        );
    }

    #[test]
    fn multiline_warning_and_error_keep_one_prefix() {
        assert_eq!(
            warning_line("line one\nline two", false),
            "Warning: line one\nline two\n"
        );
        assert_eq!(
            error_line("line one\nline two", false),
            "Error: line one\nline two\n"
        );
    }

    #[test]
    fn colorize_matrix() {
        // no_color wins over both a TTY and a forced color.
        let mut env = env_flags(true, true);
        assert!(!colorize(&env, true));
        assert!(!colorize(&env, false));
        // forced color paints even off a terminal.
        env = env_flags(false, true);
        assert!(colorize(&env, false));
        assert!(colorize(&env, true));
        // otherwise follow the stream's own TTY state.
        env = env_flags(false, false);
        assert!(colorize(&env, true));
        assert!(!colorize(&env, false));
    }

    #[test]
    fn truncation_only_when_width_known_and_overflowing() {
        // Non-TTY / verbose path never truncates.
        assert_eq!(
            headline("a very long title", Arrow::Blue, false, None).len(),
            22
        );
        // Width larger than title leaves it untouched.
        assert_eq!(
            headline("short", Arrow::Blue, false, Some(80)),
            "==> short\n"
        );
        // Width 20 -> budget 16 -> 15 chars + ellipsis.
        assert_eq!(
            headline("0123456789abcdefghij", Arrow::Blue, false, Some(20)),
            "==> 0123456789abcde\u{2026}\n"
        );
        // Rendered line (minus newline) never exceeds the terminal width.
        assert_eq!(
            headline("0123456789abcdefghij", Arrow::Blue, false, Some(20))
                .trim_end()
                .chars()
                .count(),
            20
        );
    }

    #[test]
    fn quiet_suppresses_headlines_only() {
        let (reporter, out, err) = harness(Setup {
            quiet: true,
            ..Setup::default()
        });
        reporter.ohai("headline one");
        reporter.oh1("headline two");
        reporter.print("data row");
        reporter.opoo("careful");
        reporter.onoe("boom");
        reporter.eprint("stderr note");
        assert_eq!(out.text(), "data row\n");
        assert_eq!(err.text(), "Warning: careful\nError: boom\nstderr note\n");
    }

    #[test]
    fn nonquiet_emits_headlines() {
        let (reporter, out, _err) = harness(Setup::default());
        reporter.ohai("headline");
        assert_eq!(out.text(), "==> headline\n");
    }

    #[test]
    fn stream_routing_and_verbose_flag() {
        let (reporter, out, err) = harness(Setup {
            verbose: true,
            ..Setup::default()
        });
        assert!(reporter.is_verbose());
        assert!(!reporter.is_quiet());
        reporter.print("out");
        reporter.eprint("err");
        assert_eq!(out.text(), "out\n");
        assert_eq!(err.text(), "err\n");
    }

    #[test]
    fn hint_program_reflects_construction() {
        let (default, _, _) = harness(Setup::default());
        assert_eq!(default.hint_program(), "zapbrew");
        let (brew, _, _) = harness(Setup {
            program: Some("brew"),
            ..Setup::default()
        });
        assert_eq!(brew.hint_program(), "brew");
    }

    #[test]
    fn every_error_maps_to_failure_one() {
        let errors = [
            OpError::MissingFormula {
                name: "foo".to_owned(),
            },
            OpError::Refusal {
                message: "no".to_owned(),
            },
            OpError::InvalidState {
                reason: "bad".to_owned(),
            },
            OpError::CommandFailed {
                program: "git".to_owned(),
                status: "1".to_owned(),
                stderr: "err".to_owned(),
            },
            OpError::DependencyCycle {
                cycle: vec!["a".to_owned(), "b".to_owned()],
            },
        ];
        for error in &errors {
            assert_eq!(failure_status(error), 1);
        }
    }

    #[test]
    fn exit_constants() {
        assert_eq!(SIGINT_EXIT_CODE, 130);
    }

    #[test]
    fn report_error_adds_single_prefix() {
        let (reporter, _out, err) = harness(Setup::default());
        let error = OpError::MissingFormula {
            name: "foo".to_owned(),
        };
        report_error(&reporter, &error);
        let rendered = err.text();
        assert_eq!(rendered.matches("Error: ").count(), 1);
        assert!(rendered.starts_with("Error: No available formula"));
    }

    fn env_flags(no_color: bool, color: bool) -> Env {
        use std::collections::HashMap;

        use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, EnvDetectInput};

        struct NoRun;
        impl CommandRunner for NoRun {
            fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
                unreachable!("env detection must not run host commands")
            }
        }

        let mut vars = HashMap::new();
        if no_color {
            vars.insert("HOMEBREW_NO_COLOR".to_owned(), "1".to_owned());
        }
        if color {
            vars.insert("HOMEBREW_COLOR".to_owned(), "1".to_owned());
        }
        Env::detect_from(
            &EnvDetectInput {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                home: "/tmp/zapbrew-home".into(),
                xdg_cache_home: None,
                vars,
                available_parallelism: 2,
            },
            &NoRun,
        )
        .expect("env")
    }
}
