//! Homebrew-style verbs: install, uninstall, upgrade, outdated, autoremove, cleanup, pin, deps, and related commands.

mod context;
pub mod dependency;
mod error;
pub mod fetch;
pub mod install;
pub mod platform;
pub mod reinstall;
pub mod state;
mod transaction;

pub use context::{Ctx, Reporter};
pub use error::OpError;
