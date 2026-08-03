use std::collections::HashSet;
use zapbrew_net::download_all;

use crate::dependency::{DependencyMode, DependencyOptions, EdgeFilter, expand};
use crate::install::{request_for_formula, resolve_formula};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub deps: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for requested in &args.names {
        let formula = resolve_formula(ctx, requested).await?;
        if seen.insert(formula.name.clone()) {
            roots.push(formula);
        }
    }

    let mut formulae = Vec::new();
    if args.deps {
        for dependency in expand(
            ctx.catalog.as_ref(),
            roots.iter().map(|formula| formula.name.as_str()),
            &DependencyOptions {
                target: ctx.env.bottle_tag,
                mode: DependencyMode::Pour,
                filter: EdgeFilter::ALL,
            },
        )? {
            let formula =
                ctx.catalog
                    .get(&dependency.name)
                    .ok_or_else(|| OpError::MissingFormula {
                        name: dependency.name.clone(),
                    })?;
            formulae.push(formula);
        }
    }
    formulae.extend(roots);
    let mut seen = HashSet::new();
    formulae.retain(|formula| seen.insert(formula.name.clone()));

    let requests = formulae
        .iter()
        .map(|formula| request_for_formula(ctx, formula).map(|(_, request)| request))
        .collect::<Result<Vec<_>, OpError>>()?;
    for request in &requests {
        ctx.reporter
            .ohai(&format!("Fetching {}", request.name.as_str()));
        ctx.reporter
            .oh1(&format!("Downloading {}", request.bottle.url));
    }
    let cached = download_all(&ctx.env, &ctx.http, requests.clone()).await?;
    for (request, cached) in requests.into_iter().zip(cached) {
        if cached.reused {
            ctx.reporter
                .print(&format!("Already downloaded: {}", cached.path));
        } else {
            ctx.reporter
                .print(&format!("Downloaded to: {}", cached.path));
        }
        ctx.reporter
            .print(&format!("SHA-256: {}", request.bottle.sha256));
    }
    Ok(())
}
