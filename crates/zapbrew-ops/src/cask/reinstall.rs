use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_api::Cask;
use zapbrew_prefix::LockGuard;

use super::transaction::{
    DeployedArtifact, InstallRecord, Replacement, installed_version_dirs, read_record,
};
use super::{acquire_locks, approved_appdir, artifact, resolve};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub tokens: Vec<String>,
    pub appdir: Option<Utf8PathBuf>,
}

/// A set of cask installs prepared under their token locks, ready for the
/// force-reinstall transaction.
pub(crate) struct LockedCasks<'a> {
    prepared: Vec<super::install::Prepared<'a>>,
    _locks: Vec<LockGuard>,
}

pub(crate) struct ValidatedCasks<'a> {
    prepared: Vec<super::install::Prepared<'a>>,
    downloads: zapbrew_net::PreparedArtifactDownloads,
    _locks: Vec<LockGuard>,
}

pub(crate) struct DownloadedCasks<'a> {
    prepared: Vec<super::install::Prepared<'a>>,
    cached: Vec<zapbrew_net::CachedArtifact>,
    _locks: Vec<LockGuard>,
}

pub(crate) struct LockedTokens<'a> {
    casks: Vec<&'a Cask>,
    locks: Vec<LockGuard>,
}

impl<'a> LockedTokens<'a> {
    pub(crate) fn prepare(
        self,
        ctx: &'a Ctx,
        tokens: &BTreeSet<String>,
        appdir: Option<&Utf8Path>,
        purpose: Purpose,
    ) -> Result<LockedCasks<'a>, OpError> {
        let casks = self
            .casks
            .into_iter()
            .filter(|cask| tokens.contains(&cask.token))
            .collect();
        let prepared = prepare_validated(ctx, casks, appdir, purpose)?;
        Ok(LockedCasks {
            prepared,
            _locks: self.locks,
        })
    }
}

impl<'a> LockedCasks<'a> {
    pub(crate) async fn execute(self, ctx: &Ctx) -> Result<(), OpError> {
        for prepared in self.prepared {
            super::install::execute(ctx, prepared, Replacement::Replace).await?;
        }
        Ok(())
    }

    pub(crate) fn validate_downloads(self, ctx: &Ctx) -> Result<ValidatedCasks<'a>, OpError> {
        let downloads =
            zapbrew_net::prepare_artifact_downloads(&ctx.env, download_requests(&self.prepared))?;
        Ok(ValidatedCasks {
            prepared: self.prepared,
            downloads,
            _locks: self._locks,
        })
    }
}

impl<'a> ValidatedCasks<'a> {
    pub(crate) async fn download(self, ctx: &Ctx) -> Result<DownloadedCasks<'a>, OpError> {
        let cached =
            zapbrew_net::download_artifacts_all(&ctx.env, &ctx.http, self.downloads).await?;
        Ok(DownloadedCasks {
            prepared: self.prepared,
            cached,
            _locks: self._locks,
        })
    }
}

impl DownloadedCasks<'_> {
    pub(crate) async fn execute(self, ctx: &Ctx) -> Result<(), OpError> {
        for (prepared, cached) in self.prepared.into_iter().zip(&self.cached) {
            super::install::execute_cached(ctx, prepared, Replacement::Replace, cached).await?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Purpose {
    Reinstall,
    Upgrade,
}

impl Purpose {
    fn past(self) -> &'static str {
        match self {
            Self::Reinstall => "reinstalled",
            Self::Upgrade => "upgraded",
        }
    }

    fn gerund(self) -> &'static str {
        match self {
            Self::Reinstall => "reinstalling",
            Self::Upgrade => "upgrading",
        }
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    preflight(
        ctx,
        &args.tokens,
        args.appdir.as_deref(),
        Purpose::Reinstall,
    )?
    .execute(ctx)
    .await
}
pub(crate) fn validate(
    ctx: &Ctx,
    tokens: &[String],
    appdir: Option<&Utf8Path>,
    purpose: Purpose,
) -> Result<(), OpError> {
    if let Some(path) = appdir {
        validate_appdir(ctx, path)?;
    }
    let (_, casks) = resolve_casks(ctx, tokens)?;
    let prepared = prepare_validated(ctx, casks, appdir, purpose)?;
    validate_downloads(ctx, &prepared)
}

/// Acquire one canonical token lock per cask, validate every installed record,
/// resolve the effective appdir per token, and plan the new artifacts. No
/// network or prefix mutation.
pub(crate) fn preflight<'a>(
    ctx: &'a Ctx,
    tokens: &[String],
    appdir: Option<&Utf8Path>,
    purpose: Purpose,
) -> Result<LockedCasks<'a>, OpError> {
    let selected = tokens.iter().cloned().collect::<BTreeSet<_>>();
    lock_tokens(ctx, tokens, appdir)?.prepare(ctx, &selected, appdir, purpose)
}

pub(crate) fn lock_tokens<'a>(
    ctx: &'a Ctx,
    tokens: &[String],
    appdir: Option<&Utf8Path>,
) -> Result<LockedTokens<'a>, OpError> {
    if let Some(path) = appdir {
        validate_appdir(ctx, path)?;
    }
    let (seen_tokens, casks) = resolve_casks(ctx, tokens)?;
    let locks = acquire_locks(ctx, &seen_tokens)?;
    Ok(LockedTokens { casks, locks })
}

fn resolve_casks<'a>(
    ctx: &'a Ctx,
    tokens: &[String],
) -> Result<(BTreeSet<String>, Vec<&'a Cask>), OpError> {
    let mut canonical_casks = Vec::with_capacity(tokens.len());
    let mut seen_tokens = BTreeSet::new();
    for requested in tokens {
        let cask = resolve(ctx, requested)?;
        if seen_tokens.insert(cask.token.clone()) {
            canonical_casks.push(cask);
        }
    }
    Ok((seen_tokens, canonical_casks))
}

fn prepare_validated<'a>(
    ctx: &'a Ctx,
    canonical_casks: Vec<&'a Cask>,
    appdir: Option<&Utf8Path>,
    purpose: Purpose,
) -> Result<Vec<super::install::Prepared<'a>>, OpError> {
    let recorded = canonical_casks
        .into_iter()
        .map(|cask| Ok((cask, recorded_appdirs_for_token(ctx, cask, purpose)?)))
        .collect::<Result<Vec<_>, OpError>>()?;
    let mut prepared = Vec::with_capacity(recorded.len());
    for (cask, recorded_appdirs) in recorded {
        let effective_appdir = match appdir {
            Some(path) => path.to_path_buf(),
            None => common_recorded_appdir(cask, recorded_appdirs)?,
        };
        let prepared_cask = super::install::prepare(ctx, cask, &effective_appdir)?;
        if prepared_cask
            .plan
            .actions
            .iter()
            .any(|action| matches!(action, artifact::Action::Pkg { .. }))
        {
            return Err(OpError::Refusal {
                message: format!(
                    "Cask '{}' would install an irreversible pkg; {} it is not supported.",
                    cask.token,
                    purpose.gerund()
                ),
            });
        }
        prepared.push(prepared_cask);
    }
    Ok(prepared)
}

fn download_requests(
    prepared: &[super::install::Prepared<'_>],
) -> Vec<zapbrew_net::ArtifactDownloadRequest> {
    prepared
        .iter()
        .map(|prepared| zapbrew_net::ArtifactDownloadRequest {
            url: prepared.spec.url.clone(),
            alias_name: prepared.spec.alias_name.clone(),
            sha256: prepared.spec.checksum.clone(),
        })
        .collect()
}

fn validate_downloads(ctx: &Ctx, prepared: &[super::install::Prepared<'_>]) -> Result<(), OpError> {
    zapbrew_net::prepare_artifact_downloads(&ctx.env, download_requests(prepared))?;
    Ok(())
}

pub(crate) fn validate_appdir(ctx: &Ctx, path: &Utf8Path) -> Result<(), OpError> {
    if !approved_appdir(&ctx.env, path) {
        return Err(OpError::Refusal {
            message: format!("Cask appdir '{path}' is outside approved roots."),
        });
    }
    let anchor = if path.starts_with(Utf8Path::new(crate::cask::DEFAULT_APPDIR)) {
        Utf8Path::new(crate::cask::DEFAULT_APPDIR)
    } else if path.starts_with(&ctx.env.home) {
        ctx.env.home.as_path()
    } else {
        ctx.env.prefix.as_path()
    };
    artifact::confined_target_physical(ctx, &path.join("zapbrew-appdir-probe"), &[anchor])
}

/// Load every installed record for `cask`, validate it is reversible, and
/// return its recorded appdirs. Refuses if the token has no installed versions,
/// contains an irreversible pkg, or has nonempty uninstall directives.
fn recorded_appdirs_for_token(
    ctx: &Ctx,
    cask: &Cask,
    purpose: Purpose,
) -> Result<BTreeSet<Utf8PathBuf>, OpError> {
    let token_dir = ctx.env.caskroom.join(&cask.token);
    let versions = installed_version_dirs(&token_dir)?;
    if versions.is_empty() {
        return Err(OpError::Refusal {
            message: format!("Cask '{}' is not installed.", cask.token),
        });
    }

    let mut records = Vec::with_capacity(versions.len());
    for version_dir in &versions {
        records.push(read_record(ctx, &cask.token, version_dir)?);
    }

    for record in &records {
        if record
            .artifacts
            .iter()
            .any(|a| matches!(a, DeployedArtifact::Pkg { .. }))
        {
            return Err(OpError::Refusal {
                message: format!(
                    "Cask '{}' has an irreversible pkg install and cannot be {}.",
                    cask.token,
                    purpose.past()
                ),
            });
        }
        if !record.uninstall.is_empty() {
            return Err(OpError::Refusal {
                message: format!(
                    "Cask '{}' has nonempty uninstall directives and cannot be {}.",
                    cask.token,
                    purpose.past()
                ),
            });
        }
    }

    Ok(records.iter().map(InstallRecord::appdir).collect())
}

fn common_recorded_appdir(
    cask: &Cask,
    appdirs: BTreeSet<Utf8PathBuf>,
) -> Result<Utf8PathBuf, OpError> {
    if appdirs.len() > 1 {
        return Err(OpError::Refusal {
            message: format!(
                "Cask '{}' has installed versions with different appdirs ({}); \
                 use --appdir to choose one.",
                cask.token,
                appdirs
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    Ok(appdirs
        .into_iter()
        .next()
        .unwrap_or_else(|| Utf8PathBuf::from(crate::cask::DEFAULT_APPDIR)))
}
