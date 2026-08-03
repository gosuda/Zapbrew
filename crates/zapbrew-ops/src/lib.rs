//! Homebrew-style verbs: install, uninstall, upgrade, outdated, autoremove, cleanup, pin, deps, and related commands.

mod context;
pub mod dependency;
mod error;
pub mod platform;
pub mod state;

pub use context::{Ctx, Reporter};
pub use error::OpError;
