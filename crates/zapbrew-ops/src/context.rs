use std::sync::Arc;

use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_prefix::{CommandRunner, Env};

/// User-visible output boundary owned by the operations crate.
pub trait Reporter: Send + Sync {
    fn ohai(&self, message: &str);
    fn oh1(&self, message: &str);
    fn opoo(&self, message: &str);
    fn onoe(&self, message: &str);
    fn print(&self, message: &str);
    fn eprint(&self, message: &str);

    /// Program name used in self-referential hints such as
    /// `<prog> reinstall foo`. Defaults to `zapbrew`; a terminal reporter that
    /// was invoked through the brew shim overrides it with `brew`.
    fn hint_program(&self) -> &str {
        "zapbrew"
    }
}

/// Shared dependencies for every operation entry point.
pub struct Ctx {
    pub env: Env,
    pub http: reqwest::Client,
    pub catalog: Arc<Catalog>,
    pub casks: Arc<CaskCatalog>,
    pub commands: Arc<dyn CommandRunner>,
    pub reporter: Arc<dyn Reporter>,
}
