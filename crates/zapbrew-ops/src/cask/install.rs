use std::collections::BTreeSet;

use camino::Utf8PathBuf;

use super::artifact;
use super::transaction;
use super::{acquire_locks, require_macos, resolve};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub tokens: Vec<String>,
    pub appdir: Option<Utf8PathBuf>,
    pub force: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    require_macos(ctx)?;
    let casks = args
        .tokens
        .iter()
        .map(|token| resolve(ctx, token))
        .collect::<Result<Vec<_>, _>>()?;
    let canonical = casks
        .iter()
        .map(|cask| cask.token.clone())
        .collect::<BTreeSet<_>>();
    let _locks = acquire_locks(ctx, &canonical)?;
    let appdir = args
        .appdir
        .unwrap_or_else(|| Utf8PathBuf::from(crate::cask::DEFAULT_APPDIR));
    if !crate::cask::approved_appdir(&ctx.env, &appdir) {
        return Err(OpError::Refusal {
            message: format!("Cask appdir '{appdir}' is outside approved roots."),
        });
    }

    // Full artifact and download preflight for every token before any I/O.
    let prepared = casks
        .into_iter()
        .map(|cask| {
            let plan = artifact::plan(ctx, cask, &appdir)?;
            let (version, url, checksum) = transaction::validate_download(cask)?;
            Ok((cask, plan, version, url, checksum))
        })
        .collect::<Result<Vec<_>, OpError>>()?;

    for (cask, plan, version, url, checksum) in prepared {
        transaction::install(
            ctx,
            transaction::CaskInstall {
                cask,
                plan: &plan,
                version: &version,
                url: &url,
                checksum: checksum.as_ref(),
                appdir: &appdir,
                force: args.force,
            },
        )
        .await?;
    }
    Ok(())
}
