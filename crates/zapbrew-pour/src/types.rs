use camino::Utf8PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkOptions {
    pub overwrite: bool,
    pub dry_run: bool,
    pub force: bool,
    pub keg_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkReport {
    pub linked: Vec<Utf8PathBuf>,
    pub conflicts: Vec<Utf8PathBuf>,
    pub backups: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnlinkReport {
    pub removed: Vec<Utf8PathBuf>,
    pub pruned: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelocationReport {
    pub changed_files: Vec<Utf8PathBuf>,
}
