use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zapbrew_api::Cask;
use zapbrew_types::Checksum;

use super::archive;
use super::artifact::{self, Action, Plan, Reverse};
use super::{path_exists, remove_entry, safe_lexical, unique_stage};
use crate::{Ctx, OpError};

/// Filename of the typed install record stored inside a Caskroom version tree.
/// It is the single source of truth for every zapbrew cask removal or
/// replacement; the raw `Casks/<token>.json` receipt is written for Homebrew
/// interoperability only and is never parsed back.
pub(super) const RECORD_FILE: &str = ".zapbrew-record.json";

/// One deployed artifact target recorded at install time, in application order.
/// Paths persist as plain strings; they are re-typed and re-validated on load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DeployedArtifact {
    /// A copied or moved payload (app/suite/font/plugin/completion/artifact).
    Path { target: String },
    /// A prefix symlink (binary/manpage) pointing at the promoted keg path.
    Symlink { target: String },
    /// An irreversible pkg install, kept only for honest leftover reporting.
    Pkg { source: String },
}

/// Schema-1 typed install record persisted per installed cask version.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallRecord {
    pub schema: u32,
    pub token: String,
    pub version: String,
    pub appdir: String,
    pub artifacts: Vec<DeployedArtifact>,
    pub uninstall: Vec<Value>,
    pub zap: Vec<Value>,
}

impl InstallRecord {
    /// Build the record purely from a validated plan; performs no I/O.
    pub(super) fn from_plan(plan: &Plan, token: &str, version: &str, appdir: &Utf8Path) -> Self {
        let artifacts = plan
            .actions
            .iter()
            .map(|action| match action {
                Action::Move { target, .. } | Action::Copy { target, .. } => {
                    DeployedArtifact::Path {
                        target: target.to_string(),
                    }
                }
                Action::Symlink { target, .. } => DeployedArtifact::Symlink {
                    target: target.to_string(),
                },
                Action::Pkg { source } => DeployedArtifact::Pkg {
                    source: source.clone(),
                },
            })
            .collect();
        Self {
            schema: 1,
            token: token.to_owned(),
            version: version.to_owned(),
            appdir: appdir.to_string(),
            artifacts,
            uninstall: plan.uninstall.clone(),
            zap: plan.zap.clone(),
        }
    }

    /// The install-time appdir, re-typed after validation.
    pub(super) fn appdir(&self) -> Utf8PathBuf {
        Utf8PathBuf::from(&self.appdir)
    }

    /// Every reversible deployed target, in application order.
    pub(super) fn targets(&self) -> impl Iterator<Item = Utf8PathBuf> + '_ {
        self.artifacts.iter().filter_map(|artifact| match artifact {
            DeployedArtifact::Path { target } | DeployedArtifact::Symlink { target } => {
                Some(Utf8PathBuf::from(target.as_str()))
            }
            DeployedArtifact::Pkg { .. } => None,
        })
    }
}

/// Write the record into the staged version tree before artifact application;
/// atomic promotion carries it into `Caskroom/<token>/<version>`.
pub(super) fn write_record(staging: &Utf8Path, record: &InstallRecord) -> Result<(), OpError> {
    let path = staging.join(RECORD_FILE);
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|source| OpError::InvalidState {
        reason: format!("could not serialize cask install record: {source}"),
    })?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|source| OpError::io("write", &path, source))
}

/// Read and validate the install record for a promoted version tree. Loading is
/// a trust boundary: the record file and the version root are inspected with
/// no-follow metadata, deserialization is fail-closed (unknown fields, wrong
/// schema, or a token/version mismatch are typed `InvalidState`), and every
/// deserialized target path must stay inside its allowed root. There is no
/// fallback or migration in this clean cutover.
pub(super) fn read_record(
    ctx: &Ctx,
    token: &str,
    version_dir: &Utf8Path,
) -> Result<InstallRecord, OpError> {
    let version_meta = fs::symlink_metadata(version_dir)
        .map_err(|source| OpError::io("inspect", version_dir, source))?;
    if version_meta.file_type().is_symlink() || !version_meta.is_dir() {
        return Err(OpError::InvalidState {
            reason: format!("cask version root {version_dir} is not a real directory"),
        });
    }
    let path = version_dir.join(RECORD_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(OpError::InvalidState {
                reason: format!("cask install record {path} is missing"),
            });
        }
        Err(source) => return Err(OpError::io("read", &path, source)),
    };
    let record_meta =
        fs::symlink_metadata(&path).map_err(|source| OpError::io("inspect", &path, source))?;
    if record_meta.file_type().is_symlink() || !record_meta.is_file() {
        return Err(OpError::InvalidState {
            reason: format!("cask install record {path} is not a regular file"),
        });
    }
    let text = std::str::from_utf8(&bytes).map_err(|source| OpError::InvalidState {
        reason: format!("cask install record {path} is not UTF-8: {source}"),
    })?;
    let record: InstallRecord =
        serde_json::from_str(text).map_err(|source| OpError::InvalidState {
            reason: format!("cask install record {path} is malformed: {source}"),
        })?;
    let version = version_dir
        .file_name()
        .ok_or_else(|| OpError::InvalidState {
            reason: format!("installed cask version has no basename: {version_dir}"),
        })?;
    record.validate(ctx, token, version, &path)?;
    Ok(record)
}

impl InstallRecord {
    fn validate(
        &self,
        ctx: &Ctx,
        token: &str,
        version: &str,
        path: &Utf8Path,
    ) -> Result<(), OpError> {
        let invalid = |reason: String| OpError::InvalidState {
            reason: format!("cask install record {path}: {reason}"),
        };
        if self.schema != 1 {
            return Err(invalid(format!("unsupported schema {}", self.schema)));
        }
        if self.token != token || self.version != version {
            return Err(invalid(format!(
                "record is for '{} {}', expected '{token} {version}'",
                self.token, self.version
            )));
        }
        let appdir = self.appdir();
        if !appdir.is_absolute() || !safe_lexical(appdir.as_std_path()) {
            return Err(invalid(format!("unsafe appdir '{appdir}'")));
        }
        for target in self.targets() {
            if !target.is_absolute() || !safe_lexical(target.as_std_path()) {
                return Err(invalid(format!("unsafe target path '{target}'")));
            }
            let confined = target.starts_with(&ctx.env.home)
                || target.starts_with(&ctx.env.prefix)
                || target.starts_with(&appdir);
            if !confined {
                return Err(invalid(format!(
                    "target path '{target}' is outside approved roots"
                )));
            }
        }
        Ok(())
    }
}

/// One fully preflighted cask install request.
pub(super) struct CaskInstall<'a> {
    pub cask: &'a Cask,
    pub plan: &'a Plan,
    pub version: &'a str,
    pub url: &'a str,
    pub checksum: Option<&'a Checksum>,
    pub appdir: &'a Utf8Path,
    pub force: bool,
}

/// Execute one fully preflighted install as a journaled whole-token
/// transaction. With `force`, every installed version of the token collapses to
/// the new version: all old records are validated before any mutation, every
/// deployed target and old version/metadata tree is backed up once, and a
/// single atomic promote rename is the only commit point.
pub(super) async fn install(ctx: &Ctx, request: CaskInstall<'_>) -> Result<(), OpError> {
    let CaskInstall {
        cask,
        plan,
        version,
        url,
        checksum,
        appdir,
        force,
    } = request;
    let token_dir = ctx.env.caskroom.join(&cask.token);
    let final_dir = token_dir.join(version);
    let installed = installed_version_dirs(&token_dir)?;
    if !force && !installed.is_empty() {
        ctx.reporter
            .opoo(&format!("Cask '{}' is already installed.", cask.token));
        return Ok(());
    }

    let cached = zapbrew_net::fetch_artifact(&ctx.env, &ctx.http, url, checksum).await?;
    let staging = unique_stage(ctx, &cask.token)?;
    let mut journal = Vec::<Reverse>::new();
    let mut receipt = None;
    let mut backup_root = None;

    let result = (|| {
        archive::extract(ctx, &cached.path, url, &staging)?;

        if !installed.is_empty() {
            // Whole-token force replacement: load and validate every old record
            // before any mutation, then back up each deployed target once and
            // every old version dir plus its matching metadata receipt tree.
            let mut records = Vec::new();
            for old_dir in &installed {
                records.push(read_record(ctx, &cask.token, old_dir)?);
            }
            let mut seen = BTreeSet::new();
            let targets = records
                .iter()
                .flat_map(InstallRecord::targets)
                .filter(|target| seen.insert(target.clone()))
                .collect::<BTreeSet<_>>();
            let root = unique_stage(ctx, &format!("{}-replaced", cask.token))?;
            backup_root = Some(root.clone());
            artifact::backup_targets(
                targets.iter().map(Utf8PathBuf::as_path),
                &root,
                &mut journal,
            )?;
            for old_dir in &installed {
                let name = old_dir.file_name().ok_or_else(|| OpError::InvalidState {
                    reason: format!("installed cask version has no basename: {old_dir}"),
                })?;
                let backup = root.join(format!("version-{name}"));
                fs::rename(old_dir, &backup)
                    .map_err(|source| OpError::io("backup", old_dir, source))?;
                journal.push(Reverse::RestoreDir {
                    backup,
                    original: old_dir.clone(),
                });
                let metadata = token_dir.join(".metadata").join(name);
                if path_exists(&metadata) {
                    let backup = root.join(format!("metadata-{name}"));
                    fs::rename(&metadata, &backup)
                        .map_err(|source| OpError::io("backup", &metadata, source))?;
                    journal.push(Reverse::RestoreDir {
                        backup,
                        original: metadata,
                    });
                }
            }
        }

        // Persist the typed record in the staged tree before any artifact
        // application; atomic promotion carries it into the version directory.
        let record = InstallRecord::from_plan(plan, &cask.token, version, appdir);
        write_record(&staging, &record)?;
        artifact::apply(ctx, plan, &staging, &final_dir, &mut journal)?;
        let written = write_receipt(ctx, cask, version)?;
        receipt = Some(written);
        if let Some(parent) = final_dir.parent() {
            fs::create_dir_all(parent).map_err(|source| OpError::io("create", parent, source))?;
        }
        fs::rename(&staging, &final_dir)
            .map_err(|source| OpError::io("promote", &final_dir, source))?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            artifact::drain_backups(&journal);
            if let Some(root) = backup_root {
                let _ = fs::remove_dir_all(&root);
            }
            Ok(())
        }
        Err(original) => {
            let mut leftovers = artifact::rollback(&journal);
            if let Some(path) = receipt.as_ref()
                && remove_entry(path).is_err()
                && path_exists(path)
            {
                leftovers.push(path.to_string());
            }
            if remove_entry(&staging).is_err() && path_exists(&staging) {
                leftovers.push(staging.to_string());
            }
            // Rollback restored every child out of the backup root; drop the
            // now-empty backup dir best-effort (a surviving child stays inert).
            if let Some(root) = backup_root.as_ref() {
                let _ = fs::remove_dir(root);
            }
            if leftovers.is_empty() {
                Err(original)
            } else {
                leftovers.sort();
                leftovers.dedup();
                Err(OpError::RollbackIncomplete {
                    original: Box::new(original),
                    leftovers: leftovers.join(", "),
                })
            }
        }
    }
}

fn write_receipt(ctx: &Ctx, cask: &Cask, version: &str) -> Result<Utf8PathBuf, OpError> {
    let timestamp = Timestamp::now().strftime("%Y%m%d%H%M%S").to_string();
    let path = ctx
        .env
        .caskroom
        .join(&cask.token)
        .join(".metadata")
        .join(version)
        .join(timestamp)
        .join("Casks")
        .join(format!("{}.json", cask.token));
    let parent = path.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("receipt path has no parent: {path}"),
    })?;
    fs::create_dir_all(parent).map_err(|source| OpError::io("create", parent, source))?;
    let mut bytes =
        serde_json::to_vec_pretty(&cask.raw).map_err(|source| OpError::InvalidState {
            reason: format!("could not serialize cask receipt: {source}"),
        })?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|source| OpError::io("write", &path, source))?;
    Ok(path)
}

pub(super) fn validate_download(
    cask: &Cask,
) -> Result<(String, String, Option<Checksum>), OpError> {
    let version = cask
        .version
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::Refusal {
            message: format!("Cask '{}' has no version.", cask.token),
        })?;
    let url = cask
        .url
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::Refusal {
            message: format!("Cask '{}' has no URL.", cask.token),
        })?;
    let declared = cask.sha256.as_deref().ok_or_else(|| OpError::Refusal {
        message: format!("Cask '{}' has no checksum.", cask.token),
    })?;
    let checksum = if declared == "no_check" {
        None
    } else {
        Some(Checksum::from_str(declared).map_err(|_| OpError::Refusal {
            message: format!("Cask '{}' has an invalid checksum.", cask.token),
        })?)
    };
    Ok((version, url, checksum))
}

pub(super) fn installed_version_dirs(token_dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, OpError> {
    let entries = match fs::read_dir(token_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(OpError::io("read", token_dir, source)),
    };
    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| OpError::io("read", token_dir, source))?;
        let path =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("non-UTF-8 Caskroom path: {}", path.display()),
            })?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| OpError::io("inspect", &path, source))?;
        let name = path.file_name().unwrap_or_default();
        if metadata.is_dir() && !metadata.file_type().is_symlink() && !name.starts_with('.') {
            versions.push(path);
        }
    }
    versions.sort();
    Ok(versions)
}
