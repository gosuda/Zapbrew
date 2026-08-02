//! Shared parse error for zapbrew-types `FromStr` impls.

/// `FromStr` failure embedding the offending input string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TypeError {
    #[error("invalid checksum: {0}")]
    InvalidChecksum(String),
    #[error("invalid bottle tag: {0}")]
    InvalidBottleTag(String),
    #[error("invalid macOS version: {0}")]
    InvalidMacOsVersion(String),
    #[error("invalid formula name: {0}")]
    InvalidFormulaName(String),
    #[error("invalid package version: {0}")]
    InvalidPkgVersion(String),
}
