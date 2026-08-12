use camino::Utf8PathBuf;
use zapbrew_types::BottleTag;

use super::artifact;
use super::transaction;
use super::{acquire_locks, resolve};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub tokens: Vec<String>,
    pub appdir: Option<Utf8PathBuf>,
    pub force: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let casks = args
        .tokens
        .iter()
        .map(|token| resolve(ctx, token))
        .collect::<Result<Vec<_>, _>>()?;
    for cask in &casks {
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
    }
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
    // Plan every artifact first; no token may reach staging or download until
    // every plan is a reversible or `stage_only` Linux plan or a supported
    // macOS plan.
    let planned = casks
        .into_iter()
        .map(|cask| {
            let plan = artifact::plan(ctx, cask, &appdir)?;
            Ok((cask, plan))
        })
        .collect::<Result<Vec<_>, OpError>>()?;

    // Linux .dmg pre-download boundary: refuse after planning succeeds and
    // before any network request or staging directory is created.
    if !matches!(ctx.env.bottle_tag, BottleTag::MacOs { .. }) {
        for (cask, _) in &planned {
            if let Some(url) = cask.url.as_deref()
                && super::archive_suffix(url).ends_with(".dmg")
            {
                return Err(OpError::Refusal {
                    message: format!(
                        "Cask '{}' ships a macOS disk image, which is unavailable on Linux.",
                        cask.token
                    ),
                });
            }
        }
    }

    // Download preflight for every token after artifact and .dmg boundaries.
    let prepared = planned
        .into_iter()
        .map(|(cask, plan)| {
            let spec = super::download_spec(cask).map_err(|problem| OpError::Refusal {
                message: problem.message(),
            })?;
            Ok((cask, plan, spec))
        })
        .collect::<Result<Vec<_>, OpError>>()?;

    // All pure validation is complete; only now take per-token locks.
    let canonical = prepared
        .iter()
        .map(|(cask, _, _)| cask.token.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let _locks = acquire_locks(ctx, &canonical)?;

    for (cask, plan, spec) in prepared {
        transaction::install(
            ctx,
            transaction::CaskInstall {
                cask,
                plan: &plan,
                version: &spec.version,
                url: &spec.url,
                checksum: spec.checksum.as_ref(),
                alias_name: &spec.alias_name,
                appdir: &appdir,
                force: args.force,
            },
        )
        .await?;
    }
    Ok(())
}
