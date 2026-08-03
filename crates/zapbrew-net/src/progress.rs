//! Indicatif progress gated on interactive stderr.

use std::io::IsTerminal;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use zapbrew_prefix::Env;

/// Create a download progress bar, or a hidden bar off a terminal.
///
/// Quiet-mode suppression belongs to the CLI, which can hide or redirect the
/// progress stream. Color and emoji preferences do not disable byte progress.
pub(crate) fn download_progress(_env: &Env, total: Option<u64>) -> ProgressBar {
    let show = std::io::stderr().is_terminal();
    let bar = match total {
        Some(n) => ProgressBar::new(n),
        None => ProgressBar::new_spinner(),
    };
    if show {
        if let Ok(style) = ProgressStyle::with_template(
            "{msg} [{bar:40}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
        ) {
            bar.set_style(style.progress_chars("=> "));
        }
        bar.set_draw_target(ProgressDrawTarget::stderr());
    } else {
        bar.set_draw_target(ProgressDrawTarget::hidden());
    }
    bar
}
