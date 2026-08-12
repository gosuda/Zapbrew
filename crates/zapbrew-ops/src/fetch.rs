use std::collections::HashSet;
use zapbrew_net::{
    ArtifactDownloadRequest, PreparedArtifactDownloads, download_all, download_artifacts_all,
    prepare_artifact_downloads,
};

use crate::cask::download_spec;
use crate::dependency::{DependencyMode, DependencyOptions, EdgeFilter, expand};
use crate::install::{request_for_formula, resolve_formula};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    /// Resolve names as formulae first, then casks, and only after both miss try
    /// the lazy formula migration resolution.
    #[default]
    Auto,
    /// Treat every name as a formula and never fall back to cask.
    FormulaOnly,
    /// Treat every name as a cask and never fall back to formula.
    CaskOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub deps: bool,
    pub mode: Mode,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if matches!(args.mode, Mode::CaskOnly) && args.deps {
        return Err(OpError::Refusal {
            message: "Fetching cask dependencies is not supported.".to_owned(),
        });
    }

    let mut formula_roots = Vec::new();
    let mut cask_roots = Vec::new();
    let mut seen_formulae = HashSet::new();
    let mut seen_casks = HashSet::new();

    for requested in &args.names {
        match args.mode {
            Mode::FormulaOnly => {
                let formula = resolve_formula(ctx, requested).await?;
                if seen_formulae.insert(formula.name.clone()) {
                    formula_roots.push(formula);
                }
            }
            Mode::CaskOnly => {
                let cask = ctx.casks.get(requested).ok_or_else(|| OpError::Refusal {
                    message: format!("Cask '{requested}' is unavailable."),
                })?;
                if seen_casks.insert(cask.token.clone()) {
                    cask_roots.push(cask);
                }
            }
            Mode::Auto => {
                // Synchronous formula exact/alias/oldname first.
                let formula = match ctx.catalog.resolve(requested) {
                    zapbrew_api::Resolution::Exact => ctx.catalog.get(requested),
                    zapbrew_api::Resolution::Alias { ref real } => ctx.catalog.get(real),
                    zapbrew_api::Resolution::Oldname { ref new } => ctx.catalog.get(new),
                    zapbrew_api::Resolution::Missing { .. } => None,
                };
                if let Some(formula) = formula {
                    if seen_formulae.insert(formula.name.clone()) {
                        formula_roots.push(formula);
                    }
                    continue;
                }

                // Synchronous cask token/old-token second.
                if let Some(cask) = ctx.casks.get(requested) {
                    if seen_casks.insert(cask.token.clone()) {
                        cask_roots.push(cask);
                    }
                    continue;
                }

                // Only after both miss do the lazy formula migration I/O.
                let formula = resolve_formula(ctx, requested).await?;
                if seen_formulae.insert(formula.name.clone()) {
                    formula_roots.push(formula);
                }
            }
        }
    }

    // Expand formula dependencies when requested; casks never expand.
    let mut formulae = Vec::new();
    if args.deps {
        for dependency in expand(
            ctx.catalog.as_ref(),
            formula_roots.iter().map(|formula| formula.name.as_str()),
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
    formulae.extend(formula_roots);
    let mut seen_formula_names = HashSet::new();
    formulae.retain(|formula| seen_formula_names.insert(formula.name.clone()));

    // Build cask artifact requests; unavailable metadata warns and skips,
    // invalid metadata is an error.
    let mut cask_requests = Vec::new();
    let mut cask_displays = Vec::new();
    for cask in cask_roots {
        match download_spec(cask) {
            Ok(spec) => {
                cask_displays.push((spec.token.clone(), spec.url.clone()));
                cask_requests.push(ArtifactDownloadRequest {
                    url: spec.url,
                    alias_name: spec.alias_name,
                    sha256: spec.checksum,
                });
            }
            Err(problem) if problem.is_unavailable() => {
                ctx.reporter
                    .opoo(&format!("{}; skipping fetch.", problem.message()));
            }
            Err(problem) => {
                return Err(OpError::Refusal {
                    message: problem.message(),
                });
            }
        }
    }

    // Fetch formulae.
    let formula_requests = formulae
        .iter()
        .map(|formula| request_for_formula(ctx, formula).map(|(_, request)| request))
        .collect::<Result<Vec<_>, OpError>>()?;

    // Preflight the cask batch after all request construction and before any
    // formula reporter/download side effect.
    let prepared_casks: Option<PreparedArtifactDownloads> = if cask_requests.is_empty() {
        None
    } else {
        Some(prepare_artifact_downloads(&ctx.env, cask_requests)?)
    };

    for request in &formula_requests {
        ctx.reporter
            .ohai(&format!("Fetching {}", request.name.as_str()));
        if ctx.reporter.is_verbose() {
            ctx.reporter
                .oh1(&format!("Downloading {}", request.bottle.url));
        }
    }
    let cached_formulae = if formula_requests.is_empty() {
        Vec::new()
    } else {
        download_all(&ctx.env, &ctx.http, formula_requests.clone()).await?
    };
    for (request, cached) in formula_requests.into_iter().zip(cached_formulae) {
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

    // Fetch casks.
    for (token, url) in &cask_displays {
        ctx.reporter.ohai(&format!("Fetching {token}"));
        if ctx.reporter.is_verbose() {
            ctx.reporter.oh1(&format!("Downloading {url}"));
        }
    }
    let cached_casks = if let Some(prepared) = prepared_casks {
        download_artifacts_all(&ctx.env, &ctx.http, prepared).await?
    } else {
        Vec::new()
    };
    for cached in cached_casks {
        if cached.reused {
            ctx.reporter
                .print(&format!("Already downloaded: {}", cached.path));
        } else {
            ctx.reporter
                .print(&format!("Downloaded to: {}", cached.path));
        }
        ctx.reporter.print(&format!("SHA-256: {}", cached.sha256));
    }

    Ok(())
}
