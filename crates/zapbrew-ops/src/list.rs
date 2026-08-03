use std::collections::BTreeSet;
use std::str::FromStr;

use zapbrew_api::Resolution;
use zapbrew_types::FormulaName;

use crate::render::columns;
use crate::state::{scan, scan_selected};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub versions: bool,
    pub oneline: bool,
    /// Output width supplied by the caller. Zero forces one item per line.
    pub width: usize,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.names.is_empty() {
        return list_installed(ctx, &args);
    }

    let names = args
        .names
        .iter()
        .map(|requested| canonical_name(ctx, requested))
        .collect::<Result<Vec<_>, _>>()?;
    let selected = names.iter().cloned().collect::<BTreeSet<_>>();
    let state = scan_selected(&ctx.env, &selected)?;

    for name in names {
        let installed = state
            .formula(&name)
            .ok_or_else(|| missing_keg(ctx, &name))?;
        let keg = installed
            .linked()
            .or_else(|| installed.latest())
            .ok_or_else(|| missing_keg(ctx, &name))?;
        for relative in keg.files() {
            ctx.reporter.print(keg.path().join(relative).as_str());
        }
    }
    Ok(())
}

fn list_installed(ctx: &Ctx, args: &Args) -> Result<(), OpError> {
    let state = scan(&ctx.env)?;
    if args.versions {
        for installed in state.iter() {
            let versions = installed
                .kegs()
                .iter()
                .map(|keg| keg.version().to_string())
                .collect::<Vec<_>>()
                .join(" ");
            ctx.reporter
                .print(&format!("{} {versions}", installed.name()));
        }
        return Ok(());
    }

    let names = state
        .iter()
        .map(|formula| formula.name().name().to_owned())
        .collect::<Vec<_>>();
    let rendered = columns(&names, if args.oneline { 0 } else { args.width });
    if !rendered.is_empty() {
        ctx.reporter.print(&rendered);
    }
    Ok(())
}

fn canonical_name(ctx: &Ctx, requested: &str) -> Result<String, OpError> {
    match ctx.catalog.resolve(requested) {
        Resolution::Exact => ctx
            .catalog
            .get(requested)
            .map(|formula| formula.name.clone())
            .ok_or_else(|| OpError::InvalidState {
                reason: format!("catalog resolved {requested} without a formula"),
            }),
        Resolution::Alias { real } => Ok(real),
        Resolution::Oldname { new } => Ok(new),
        Resolution::Missing { .. } => FormulaName::from_str(requested)
            .map(|name| name.name().to_owned())
            .map_err(|source| OpError::InvalidState {
                reason: format!("formula name {requested} is invalid: {source}"),
            }),
    }
}

fn missing_keg(ctx: &Ctx, name: &str) -> OpError {
    OpError::Refusal {
        message: format!("No such keg: {}/{name}", ctx.env.cellar),
    }
}
