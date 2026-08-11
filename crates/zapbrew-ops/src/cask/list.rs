use std::fs;

use super::resolve;
use super::transaction::installed_version_dirs;
use crate::render::columns;
use crate::{Ctx, OpError};
use camino::{Utf8Path, Utf8PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub tokens: Vec<String>,
    pub versions: bool,
    pub one_per_line: bool,
    pub width: usize,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.tokens.is_empty() {
        let installed = scan_tokens(&ctx.env.caskroom)?;
        if args.versions {
            for token in installed {
                print_versions(ctx, &token)?;
            }
        } else {
            let rendered = columns(&installed, if args.one_per_line { 0 } else { args.width });
            if !rendered.is_empty() {
                ctx.reporter.print(&rendered);
            }
        }
        return Ok(());
    }

    for requested in &args.tokens {
        let cask = resolve(ctx, requested)?;
        let versions = installed_version_dirs(&ctx.env.caskroom.join(&cask.token))?;
        if versions.is_empty() {
            return Err(OpError::Refusal {
                message: format!("Cask '{}' is not installed.", cask.token),
            });
        }
        let values = versions
            .iter()
            .filter_map(|path| path.file_name().map(str::to_owned))
            .collect::<Vec<_>>();
        ctx.reporter
            .print(&format!("{} {}", cask.token, values.join(" ")));
    }
    Ok(())
}

fn print_versions(ctx: &Ctx, token: &str) -> Result<(), OpError> {
    let versions = installed_version_dirs(&ctx.env.caskroom.join(token))?
        .into_iter()
        .filter_map(|path| path.file_name().map(str::to_owned))
        .collect::<Vec<_>>();
    ctx.reporter
        .print(&format!("{token} {}", versions.join(" ")));
    Ok(())
}

fn scan_tokens(caskroom: &Utf8Path) -> Result<Vec<String>, OpError> {
    let entries = match fs::read_dir(caskroom) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(OpError::io("read", caskroom, source)),
    };
    let mut tokens = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| OpError::io("read", caskroom, source))?;
        let path =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("non-UTF-8 Caskroom path: {}", path.display()),
            })?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| OpError::io("inspect", &path, source))?;
        let Some(name) = path.file_name() else {
            continue;
        };
        if metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && !name.starts_with('.')
            && !installed_version_dirs(&path)?.is_empty()
        {
            tokens.push(name.to_owned());
        }
    }
    tokens.sort();
    Ok(tokens)
}
