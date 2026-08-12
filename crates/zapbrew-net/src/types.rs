//! Public request/result types for bottle downloads.
//!
//! This module is the single home for `CachedBottle`; the crate's cache
//! internals (see `cache.rs`) never re-define it.

use camino::Utf8PathBuf;
use zapbrew_types::{BottleFile, Checksum, FormulaName, PkgVersion};

/// One generic artifact to fetch, as handed to `fetch_artifact` or
/// `download_artifacts_all`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDownloadRequest {
    /// Source URL. The content cache path is derived solely from this value.
    pub url: String,
    /// Request-specific friendly basename under `$CACHE`.
    pub alias_name: String,
    /// Declared digest, or `None` for a `no_check` artifact.
    pub sha256: Option<Checksum>,
}

/// A generic artifact successfully fetched into the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedArtifact {
    /// Content-addressed final path under `$CACHE/downloads`.
    pub path: Utf8PathBuf,
    /// Friendly basename alias under `$CACHE`.
    pub alias: Utf8PathBuf,
    /// Actual SHA-256 digest computed from the cached content.
    pub sha256: Checksum,
    /// True when no response body was downloaded.
    pub reused: bool,
}

/// A bottle successfully fetched into the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedBottle {
    /// Content-addressed final path: `$CACHE/downloads/<sha256(url)>--<basename>`.
    pub path: Utf8PathBuf,
    /// Friendly alias path: `$CACHE/<basename>` (relative symlink into `downloads/`).
    pub alias: Utf8PathBuf,
    /// True when the cache already held a valid file and no bytes were downloaded.
    pub reused: bool,
}

/// One bottle to fetch, as handed to `download_all`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    /// Formula name, used for messages and the cache basename.
    pub name: FormulaName,
    /// Bottle descriptor from the API (tag, cellar, url, sha256).
    pub bottle: BottleFile,
    /// Stable version + revision, used in the cache basename.
    pub pkg_version: PkgVersion,
    /// Bottle rebuild number; 0 is omitted from the basename.
    pub rebuild: u32,
}
