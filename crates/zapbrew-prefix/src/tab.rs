//! INSTALL_RECEIPT.json (Tab) model and I/O.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::error::PrefixError;

/// A Homebrew `INSTALL_RECEIPT.json`, matching the current `Tab#to_json` schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    #[serde(default)]
    pub homebrew_version: Option<String>,

    #[serde(default)]
    pub used_options: Vec<String>,

    #[serde(default)]
    pub unused_options: Vec<String>,

    #[serde(default)]
    pub built_as_bottle: bool,

    #[serde(default)]
    pub poured_from_bottle: bool,

    #[serde(default)]
    pub loaded_from_api: bool,

    #[serde(default)]
    pub loaded_from_internal_api: bool,

    #[serde(default = "default_installed_on_request")]
    pub installed_on_request: bool,

    #[serde(default)]
    pub changed_files: Option<Vec<String>>,

    #[serde(default)]
    pub time: Option<i64>,

    #[serde(default)]
    pub source_modified_time: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdlib: Option<String>,

    #[serde(default = "default_compiler")]
    pub compiler: String,

    #[serde(default)]
    pub aliases: Vec<String>,

    #[serde(default)]
    pub runtime_dependencies: Option<Vec<RuntimeDependency>>,

    #[serde(default)]
    pub source: Source,

    #[serde(default)]
    pub arch: Option<String>,

    #[serde(default)]
    pub built_on: Option<BuiltOn>,
}

impl Default for Tab {
    fn default() -> Self {
        Self {
            homebrew_version: None,
            used_options: Vec::new(),
            unused_options: Vec::new(),
            built_as_bottle: false,
            poured_from_bottle: false,
            loaded_from_api: false,
            loaded_from_internal_api: false,
            installed_on_request: default_installed_on_request(),
            changed_files: None,
            time: None,
            source_modified_time: 0,
            stdlib: None,
            compiler: default_compiler(),
            aliases: Vec::new(),
            runtime_dependencies: None,
            source: Source::default(),
            arch: None,
            built_on: None,
        }
    }
}

fn default_installed_on_request() -> bool {
    false
}

fn default_compiler() -> String {
    #[cfg(target_os = "linux")]
    {
        "gcc".to_string()
    }
    #[cfg(not(target_os = "linux"))]
    {
        "clang".to_string()
    }
}

impl Tab {
    /// Load a receipt from `path`, returning defaults for a missing or corrupt file.
    pub fn load<P: AsRef<Utf8Path>>(path: P) -> Result<Tab, PrefixError> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(content) if content.trim().is_empty() => Ok(Tab::default()),
            Ok(content) => match serde_json::from_str::<Tab>(content.trim()) {
                Ok(tab) => Ok(tab),
                Err(_) => Ok(Tab::default()),
            },
            Err(err)
                if err.kind() == std::io::ErrorKind::NotFound
                    || err.kind() == std::io::ErrorKind::InvalidData =>
            {
                Ok(Tab::default())
            }
            Err(err) => Err(PrefixError::io("read", path, err)),
        }
    }

    /// Write this receipt as 2-space pretty JSON with a trailing newline,
    /// creating parent directories as needed.
    pub fn write<P: AsRef<Utf8Path>>(&self, path: P) -> Result<(), PrefixError> {
        let path = path.as_ref();
        let json = self
            .to_pretty_json()
            .map_err(|source| PrefixError::ReceiptJson {
                path: path.to_path_buf(),
                source,
            })?;
        if let Some(parent) = path.parent() {
            let parent_str = parent.as_str();
            if !parent_str.is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| PrefixError::io("create", parent, err))?;
            }
        }
        std::fs::write(path, json).map_err(|err| PrefixError::io("write", path, err))
    }

    /// Serialize this tab to 2-space indented JSON with exactly one trailing newline.
    pub fn to_pretty_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|s| format!("{s}\n"))
    }
}

/// A runtime dependency entry inside a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RuntimeDependency {
    #[serde(default)]
    pub full_name: String,

    #[serde(default)]
    pub version: String,

    #[serde(default)]
    pub revision: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottle_rebuild: Option<u32>,

    #[serde(default)]
    pub pkg_version: String,

    #[serde(default)]
    pub declared_directly: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility_version: Option<u32>,
}

/// Source metadata recorded in a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub path: Option<String>,

    #[serde(default)]
    pub tap: Option<String>,

    #[serde(default)]
    pub tap_git_head: Option<String>,

    #[serde(default = "default_spec")]
    pub spec: String,

    #[serde(default)]
    pub versions: SourceVersions,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scm_revision: Option<String>,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            path: None,
            tap: None,
            tap_git_head: None,
            spec: default_spec(),
            versions: SourceVersions::default(),
            scm_revision: None,
        }
    }
}

fn default_spec() -> String {
    "stable".to_string()
}

/// Version metadata inside the `source` object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SourceVersions {
    #[serde(default)]
    pub stable: Option<String>,

    #[serde(default)]
    pub head: Option<String>,

    #[serde(default)]
    pub version_scheme: u32,

    #[serde(default)]
    pub compatibility_version: Option<u32>,
}

/// Build-machine metadata, modeled as a map because the keys are OS-specific.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BuiltOn {
    values: BTreeMap<String, Option<String>>,
}

impl BuiltOn {
    pub fn new() -> Self {
        Self {
            values: BTreeMap::new(),
        }
    }
}

impl Deref for BuiltOn {
    type Target = BTreeMap<String, Option<String>>;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl DerefMut for BuiltOn {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}
