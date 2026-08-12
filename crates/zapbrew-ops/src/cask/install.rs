use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_api::Cask;
use zapbrew_types::BottleTag;

use super::transaction::{CaskInstall, Replacement};
use super::{CaskDownloadSpec, acquire_locks, artifact, download_spec, resolve, transaction};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub tokens: Vec<String>,
    pub appdir: Option<Utf8PathBuf>,
    pub force: bool,
}

/// A fully-preflighted cask install, ready for the journaled transaction.
pub(super) struct Prepared<'a> {
    pub(super) cask: &'a Cask,
    pub(super) plan: artifact::Plan,
    pub(super) spec: CaskDownloadSpec,
    pub(super) appdir: Utf8PathBuf,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let casks = args
        .tokens
        .iter()
        .map(|token| resolve(ctx, token))
        .collect::<Result<Vec<_>, _>>()?;

    let appdir = args
        .appdir
        .unwrap_or_else(|| Utf8PathBuf::from(crate::cask::DEFAULT_APPDIR));
    if !crate::cask::approved_appdir(&ctx.env, &appdir) {
        return Err(OpError::Refusal {
            message: format!("Cask appdir '{appdir}' is outside approved roots."),
        });
    }
    // No-follow-validate the appdir tree before any cask plans it as a sink.
    crate::cask::artifact::confined_target_physical(
        ctx,
        &appdir.join("zapbrew-appdir-probe"),
        &[&appdir],
    )?;

    let prepared = casks
        .into_iter()
        .map(|cask| prepare(ctx, cask, &appdir))
        .collect::<Result<Vec<_>, OpError>>()?;

    // All pure validation is complete; only now take per-token locks.
    let canonical = prepared
        .iter()
        .map(|p| p.cask.token.clone())
        .collect::<BTreeSet<_>>();
    let _locks = acquire_locks(ctx, &canonical)?;

    for p in prepared {
        let replacement = if args.force {
            Replacement::Replace
        } else {
            Replacement::Refuse
        };
        execute(ctx, p, replacement).await?;
    }
    Ok(())
}

/// Pure preflight for one cask: token validation, artifact plan, and download
/// spec. No locks and no network. The caller is responsible for locking and
/// for appdir approval/confinement.
pub(super) fn prepare<'a>(
    ctx: &'a Ctx,
    cask: &'a Cask,
    appdir: &Utf8Path,
) -> Result<Prepared<'a>, OpError> {
    if !super::one_normal_component(&cask.token) {
        return Err(OpError::Refusal {
            message: format!("Cask '{}' has an unsafe token.", cask.token),
        });
    }
    if cask.disabled {
        let reason = cask.disable_reason.as_deref().unwrap_or("is disabled");
        return Err(OpError::Refusal {
            message: format!("{} has been disabled because it {reason}!", cask.token),
        });
    }

    let plan = artifact::plan(ctx, cask, appdir)?;

    // Linux .dmg pre-download boundary: refuse after planning succeeds and
    // before any network request or staging directory is created.
    if !matches!(ctx.env.bottle_tag, BottleTag::MacOs { .. })
        && let Some(url) = cask.url.as_deref()
        && super::archive_suffix(url).ends_with(".dmg")
    {
        return Err(OpError::Refusal {
            message: format!(
                "Cask '{}' ships a macOS disk image, which is unavailable on Linux.",
                cask.token
            ),
        });
    }

    let spec = download_spec(cask).map_err(|problem| OpError::Refusal {
        message: problem.message(),
    })?;

    Ok(Prepared {
        cask,
        plan,
        spec,
        appdir: appdir.to_path_buf(),
    })
}

/// Execute one prepared cask install under the caller's token lock.
pub(super) async fn execute<'a>(
    ctx: &'a Ctx,
    prepared: Prepared<'a>,
    replacement: Replacement,
) -> Result<(), OpError> {
    transaction::install(
        ctx,
        CaskInstall {
            cask: prepared.cask,
            plan: &prepared.plan,
            version: &prepared.spec.version,
            url: &prepared.spec.url,
            checksum: prepared.spec.checksum.as_ref(),
            alias_name: &prepared.spec.alias_name,
            appdir: &prepared.appdir,
            replacement,
        },
    )
    .await
}
