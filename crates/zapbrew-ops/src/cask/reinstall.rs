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
impl LockedCasks<'_> {
    pub(crate) async fn execute(self, ctx: &Ctx) -> Result<(), OpError> {
        for prepared in self.prepared {
            super::install::execute(ctx, prepared, Replacement::Replace).await?;
        }
        Ok(())
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    preflight(ctx, &args.tokens, args.appdir.as_deref())?
        .execute(ctx)
        .await
}

/// Acquire one canonical token lock per cask, validate every installed record,
/// resolve the effective appdir per token, and plan the new artifacts. No
/// network, no mutation.
pub(crate) fn preflight<'a>(
    ctx: &'a Ctx,
    tokens: &[String],
    appdir: Option<&Utf8Path>,
) -> Result<LockedCasks<'a>, OpError> {
    // Resolve every requested name to its canonical cask, deduplicating old
    // tokens and repeated inputs before any lock or record I/O.
    let mut canonical_casks: Vec<&'a Cask> = Vec::with_capacity(tokens.len());
    let mut seen_tokens = BTreeSet::new();
    for requested in tokens {
        let cask = resolve(ctx, requested)?;
        if seen_tokens.insert(cask.token.clone()) {
            canonical_casks.push(cask);
        }
    }

    // Acquire one lock per canonical token and hold it through execute.
    let _locks = acquire_locks(ctx, &seen_tokens)?;

    // Validate every installed record under its token lock before considering
    // the optional appdir override.
    let recorded = canonical_casks
        .into_iter()
        .map(|cask| Ok((cask, recorded_appdirs_for_token(ctx, cask)?)))
        .collect::<Result<Vec<_>, OpError>>()?;

    if let Some(path) = appdir {
        if !approved_appdir(&ctx.env, path) {
            return Err(OpError::Refusal {
                message: format!("Cask appdir '{path}' is outside approved roots."),
            });
        }
        artifact::confined_target_physical(ctx, &path.join("zapbrew-appdir-probe"), &[path])?;
    }

    // Plan each token after all predecessor records and the override pass.
    let mut prepared = Vec::with_capacity(recorded.len());
    for (cask, recorded_appdirs) in recorded {
        let effective_appdir = match appdir {
            Some(path) => path.to_path_buf(),
            None => common_recorded_appdir(cask, recorded_appdirs)?,
        };
        let p = super::install::prepare(ctx, cask, &effective_appdir)?;
        if p.plan
            .actions
            .iter()
            .any(|a| matches!(a, artifact::Action::Pkg { .. }))
        {
            return Err(OpError::Refusal {
                message: format!(
                    "Cask '{}' would install an irreversible pkg; reinstalling it is not supported.",
                    cask.token
                ),
            });
        }
        prepared.push(p);
    }

    Ok(LockedCasks { prepared, _locks })
}

/// Load every installed record for `cask`, validate it is reversible, and
/// return its recorded appdirs. Refuses if the token has no installed versions,
/// contains an irreversible pkg, or has nonempty uninstall directives.
fn recorded_appdirs_for_token(ctx: &Ctx, cask: &Cask) -> Result<BTreeSet<Utf8PathBuf>, OpError> {
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
                    "Cask '{}' has an irreversible pkg install and cannot be reinstalled.",
                    cask.token
                ),
            });
        }
        if !record.uninstall.is_empty() {
            return Err(OpError::Refusal {
                message: format!(
                    "Cask '{}' has nonempty uninstall directives and cannot be reinstalled.",
                    cask.token
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
