use std::fs;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::OpError;

/// Filename of the typed install-state sidecar stored inside a Caskroom version
/// tree. The raw JSON receipt stays byte-faithful; this file records the
/// install-time appdir that the receipt itself does not carry.
pub(super) const STATE_FILE: &str = ".zapbrew-install-state.json";

/// Install-time state persisted beside the promoted version tree so uninstall and
/// force replacement reconstruct the exact deployed plan against the appdir that
/// was actually used at install time.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct InstallState {
    pub appdir: String,
}

/// Write the install state into the staged version tree before artifact
/// application; atomic promotion moves it into `Caskroom/<token>/<version>`.
pub(super) fn write(staging: &Utf8Path, appdir: &Utf8Path) -> Result<(), OpError> {
    let state = InstallState {
        appdir: appdir.as_str().to_owned(),
    };
    let path = staging.join(STATE_FILE);
    let mut bytes = serde_json::to_vec_pretty(&state).map_err(|source| OpError::InvalidState {
        reason: format!("could not serialize cask install state: {source}"),
    })?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|source| OpError::io("write", &path, source))
}

/// Read and validate the install state from a promoted version tree. Missing or
/// malformed state is a typed `InvalidState`; there is no `/Applications`
/// fallback in this clean cutover.
pub(super) fn read(version_dir: &Utf8Path) -> Result<Utf8PathBuf, OpError> {
    let path = version_dir.join(STATE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(OpError::InvalidState {
                reason: format!("cask install state {path} is missing"),
            });
        }
        Err(source) => return Err(OpError::io("read", &path, source)),
    };
    let text = std::str::from_utf8(&bytes).map_err(|source| OpError::InvalidState {
        reason: format!("cask install state {path} is not UTF-8: {source}"),
    })?;
    let state: InstallState =
        serde_json::from_str(text).map_err(|source| OpError::InvalidState {
            reason: format!("cask install state {path} is malformed: {source}"),
        })?;
    Ok(Utf8PathBuf::from(state.appdir))
}
