use crate::dependency::{EdgeFilter, UsesOptions, uses_with_casks};
use crate::render::columns;
use crate::state::{scan, scan_casks};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub recursive: bool,
    pub installed: bool,
    pub include_build: bool,
    pub include_test: bool,
    pub include_optional: bool,
    pub skip_recommended: bool,
    /// Output width supplied by the caller. Zero forces one item per line.
    pub width: usize,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let mut matches = uses_with_casks(
        ctx.catalog.as_ref(),
        ctx.casks.as_ref(),
        &args.names,
        &UsesOptions {
            host: ctx.env.bottle_tag,
            recursive: args.recursive,
            filter: EdgeFilter::query(
                args.include_build,
                args.include_test,
                args.include_optional,
                args.skip_recommended,
            ),
        },
    )?;
    if args.installed {
        let state = scan(&ctx.env)?;
        let cask_state = scan_casks(&ctx.env)?;
        matches.retain(|name| state.contains(name) || cask_state.cask(name).is_some());
    }

    let rendered = columns(&matches, args.width);
    if !rendered.is_empty() {
        ctx.reporter.print(&rendered);
    }
    Ok(())
}
