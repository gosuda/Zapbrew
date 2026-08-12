//! GHCR bottle download, cache naming, checksum verification, and retries.

mod cache;
mod download;
mod error;
mod progress;
mod select;
mod types;

pub use download::{
    PreparedArtifactDownloads, download_all, download_artifacts_all, fetch_artifact, fetch_bottle,
    prepare_artifact_downloads,
};
pub use error::NetError;
pub use select::select_bottle;
pub use types::{ArtifactDownloadRequest, CachedArtifact, CachedBottle, DownloadRequest};
