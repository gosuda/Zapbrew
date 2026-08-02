//! SHA-256 checksums.

use std::fmt;
use std::str::FromStr;

use crate::TypeError;

/// A SHA-256 digest, stored as its canonical 64-character lowercase
/// hexadecimal representation.
///
/// Parsing normalizes case, so `Checksum`s built from the same digest in
/// different cases compare and hash equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Checksum(String);

impl Checksum {
    /// The canonical lowercase hexadecimal representation of the digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Checksum {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Checksum {
    type Err = TypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(TypeError::InvalidChecksum(s.to_owned()));
        }
        Ok(Checksum(s.to_ascii_lowercase()))
    }
}
