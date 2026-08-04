//! Prefix layout, install receipts, pins, and file locks.

mod command;
mod env;
mod error;
mod ld;
mod path;
mod tab;

pub use command::{CommandOutput, CommandRunner, CommandSpec, SystemCommandRunner};
pub use env::{Env, EnvDetectInput, ProxyEnv, Shell};
pub use error::PrefixError;
pub use ld::{setup_preferred_gcc_libs, symlink_ld_so};
pub use path::{
    Keg, LockGuard, Prefix, Rack, is_pinned, linked_path, opt_path, pin, pin_relative_target,
    resolve_linked, resolve_opt, unpin,
};
pub use tab::{BuiltOn, RuntimeDependency, Source, SourceVersions, Tab};
