//! JWS fetch, verify, cache, serde model, and variation merge for the Homebrew JSON API.

mod catalog;
mod error;
mod jws;
mod model;
mod transport;

pub use catalog::{CaskCatalog, Catalog, RefreshReport, Resolution, force_refresh};
pub use error::ApiError;
pub use model::{
    Bottle, Cask, CaskArtifact, CaskDependsOn, Conflict, Dependency, DependencyTag, Formula,
    KegOnlyReason, UsesFromMacos,
};
pub use transport::ApiWarning;
