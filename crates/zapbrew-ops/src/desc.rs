use std::sync::Arc;

use regex::{Regex, RegexBuilder};
use zapbrew_api::{CaskCatalog, Catalog};

use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub search: Option<String>,
    pub search_name: Option<String>,
    pub search_description: Option<String>,
    pub formula: bool,
    pub cask: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let catalog = if args.cask {
        None
    } else {
        Some(ctx.catalog.clone())
    };
    let casks = if args.formula {
        None
    } else {
        Some(ctx.casks.clone())
    };

    match (&args.search, &args.search_name, &args.search_description) {
        (None, None, None) => {
            for requested in &args.names {
                describe_named(ctx, requested, catalog.as_ref(), casks.as_ref())?;
            }
        }
        (Some(pattern), None, None) | (None, Some(pattern), None) | (None, None, Some(pattern)) => {
            if !args.names.is_empty() {
                return Err(OpError::Refusal {
                    message: "search mode takes a single search term, not names".to_owned(),
                });
            }
            let matcher = build_matcher(pattern)?;
            search(ctx, &matcher, &args, catalog.as_ref(), casks.as_ref())?;
        }
        _ => unreachable!("clap group prevents multiple search flags"),
    }

    Ok(())
}

fn describe_named(
    ctx: &Ctx,
    requested: &str,
    catalog: Option<&Arc<Catalog>>,
    casks: Option<&Arc<CaskCatalog>>,
) -> Result<(), OpError> {
    if let Some(formula) = catalog.as_ref().and_then(|c| c.get(requested)) {
        if let Some(description) = &formula.desc {
            ctx.reporter
                .print(&format!("{}: {description}", formula.full_name));
        }
        return Ok(());
    }
    if let Some(cask) = casks.as_ref().and_then(|c| c.get(requested)) {
        ctx.reporter.print(&format!(
            "{}: {}",
            cask.token,
            cask.desc.as_deref().unwrap_or("")
        ));
        return Ok(());
    }
    Err(OpError::MissingFormula {
        name: requested.to_owned(),
    })
}

fn search(
    ctx: &Ctx,
    matcher: &Regex,
    args: &Args,
    catalog: Option<&Arc<Catalog>>,
    casks: Option<&Arc<CaskCatalog>>,
) -> Result<(), OpError> {
    let mut hits = Vec::new();

    if let Some(catalog) = catalog {
        for formula in catalog.iter() {
            if matches(
                args,
                matcher,
                &formula.name,
                &formula.full_name,
                formula.desc.as_deref(),
            ) {
                hits.push((
                    formula.full_name.clone(),
                    formula.desc.clone().unwrap_or_default(),
                ));
            }
        }
    }

    if let Some(casks) = casks {
        for cask in casks.iter() {
            if matches(
                args,
                matcher,
                &cask.token,
                &cask.token,
                cask.desc.as_deref(),
            ) {
                hits.push((cask.token.clone(), cask.desc.clone().unwrap_or_default()));
            }
        }
    }

    hits.sort_by(|(a, _), (b, _)| a.cmp(b));
    for (name, description) in hits {
        ctx.reporter.print(&format!("{name}: {description}"));
    }
    Ok(())
}

fn matches(
    args: &Args,
    matcher: &Regex,
    name: &str,
    full_name: &str,
    description: Option<&str>,
) -> bool {
    let name_field = if full_name.contains('/') {
        full_name
    } else {
        name
    };
    let in_name = matcher.is_match(name_field) || matcher.is_match(name);
    let in_desc = description.is_some_and(|d| matcher.is_match(d));

    if args.search_name.is_some() {
        in_name
    } else if args.search_description.is_some() {
        in_desc
    } else {
        in_name || in_desc
    }
}

fn build_matcher(pattern: &str) -> Result<Regex, OpError> {
    let (pattern, case_insensitive) = pattern
        .strip_prefix('/')
        .and_then(|p| p.strip_suffix('/'))
        .map_or((pattern, true), |p| (p, false));

    RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|err| OpError::Refusal {
            message: format!("invalid search pattern: {err}"),
        })
}
