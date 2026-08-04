//! GHCR bottle download, cache naming, checksum verification, and retries.

mod cache;
mod download;
mod error;
mod progress;
mod select;
mod types;

pub use download::{download_all, fetch_artifact, fetch_bottle};
pub use error::NetError;
pub use select::select_bottle;
pub use types::{CachedArtifact, CachedBottle, DownloadRequest};
