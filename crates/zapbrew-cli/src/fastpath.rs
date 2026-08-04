//! Zero-network fast-path resolution for the top-level path queries.
//!
//! `--prefix`, `--cellar`, `--cache`, and `--repository` answer directly from
//! the detected [`Env`], before any runtime, HTTP client, catalog, or `Ctx` is
//! constructed. Resolution is pure: it takes only the parsed [`Cli`] and the
//! [`Env`] and returns [`FastPath`] data, so every branch is exercised in unit
//! tests without touching the network or the process exit. The absence of a
//! client or catalog parameter is itself the seam proving a fast path can never
//! load a catalog or open a connection.

use zapbrew_prefix::Env;

use crate::cli::Cli;

/// Message refusing more than one simultaneous path query.
const MULTIPLE: &str = "only one of --prefix, --cellar, --cache, --repository may be given";

/// Message refusing `--cache <formula>`; the download path needs catalog
/// metadata the approved plan does not require here.
const CACHE_FORMULA: &str = "--cache with a formula name is not supported.";

/// Outcome of fast-path resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastPath {
    /// A resolved path line to print to stdout; the run exits 0.
    Print(String),
    /// A refusal message to render once through `onoe`; the run exits 1.
    Refuse(String),
    /// No path query was present; proceed to async dispatch.
    None,
}

/// Resolve the top-level fast paths against the detected environment.
///
/// A present path query always wins over any parsed subcommand. At most one of
/// the four may be given; more than one is a single refusal.
pub fn resolve(cli: &Cli, env: &Env) -> FastPath {
    let count = [
        cli.prefix.is_some(),
        cli.cellar.is_some(),
        cli.cache.is_some(),
        cli.repository.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();

    if count == 0 {
        return FastPath::None;
    }
    if count > 1 {
        return FastPath::Refuse(MULTIPLE.to_owned());
    }

    if let Some(query) = &cli.prefix {
        return FastPath::Print(prefix_line(env, query.as_deref()));
    }
    if let Some(query) = &cli.cellar {
        return FastPath::Print(cellar_line(env, query.as_deref()));
    }
    if let Some(query) = &cli.cache {
        return match query {
            Some(_) => FastPath::Refuse(CACHE_FORMULA.to_owned()),
            None => FastPath::Print(env.cache.to_string()),
        };
    }
    if let Some(query) = &cli.repository {
        return repository_line(env, query.as_deref());
    }
    FastPath::None
}

/// Bare `--prefix` prints the install prefix; `--prefix <formula>` prints the
/// formula's `opt` path with no existence check, matching brew's fast form.
fn prefix_line(env: &Env, formula: Option<&str>) -> String {
    match formula {
        Some(name) => env.prefix.join("opt").join(name).into_string(),
        None => env.prefix.to_string(),
    }
}

/// Bare `--cellar` prints the Cellar; `--cellar <formula>` prints its keg root.
fn cellar_line(env: &Env, formula: Option<&str>) -> String {
    match formula {
        Some(name) => env.cellar.join(name).into_string(),
        None => env.cellar.to_string(),
    }
}

/// Bare `--repository` prints the repository; `--repository <tap>` resolves the
/// canonical tap directory through the shared `zapbrew-ops` normalization.
fn repository_line(env: &Env, tap: Option<&str>) -> FastPath {
    match tap {
        None => FastPath::Print(env.repository.to_string()),
        Some(raw) => match zapbrew_ops::tap::repository_path(env, raw) {
            Ok(path) => FastPath::Print(path.into_string()),
            Err(err) => FastPath::Refuse(err.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use clap::Parser;
    use zapbrew_prefix::{Env, EnvDetectInput, SystemCommandRunner};

    use super::{CACHE_FORMULA, FastPath, MULTIPLE, resolve};
    use crate::cli::Cli;

    fn scratch_env() -> Env {
        Env::detect_from(
            &EnvDetectInput {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                home: "/home/test".into(),
                xdg_cache_home: None,
                vars: HashMap::from([("HOMEBREW_PREFIX".to_owned(), "/opt/zapbrew".to_owned())]),
                available_parallelism: 2,
            },
            &SystemCommandRunner,
        )
        .expect("scratch env")
    }

    fn cli(args: &[&str]) -> Cli {
        Cli::parse_from(args)
    }

    #[test]
    fn no_path_query_defers_to_dispatch() {
        let env = scratch_env();
        assert_eq!(resolve(&cli(&["zapbrew", "list"]), &env), FastPath::None);
    }

    #[test]
    fn bare_prefix_prints_prefix() {
        let env = scratch_env();
        assert_eq!(
            resolve(&cli(&["zapbrew", "--prefix"]), &env),
            FastPath::Print(env.prefix.to_string())
        );
    }

    #[test]
    fn bare_cellar_cache_repository_print_env_paths() {
        let env = scratch_env();
        assert_eq!(
            resolve(&cli(&["zapbrew", "--cellar"]), &env),
            FastPath::Print(env.cellar.to_string())
        );
        assert_eq!(
            resolve(&cli(&["zapbrew", "--cache"]), &env),
            FastPath::Print(env.cache.to_string())
        );
        assert_eq!(
            resolve(&cli(&["zapbrew", "--repository"]), &env),
            FastPath::Print(env.repository.to_string())
        );
    }

    #[test]
    fn prefix_formula_appends_opt_path() {
        let env = scratch_env();
        assert_eq!(
            resolve(&cli(&["zapbrew", "--prefix", "wget"]), &env),
            FastPath::Print(env.prefix.join("opt").join("wget").into_string())
        );
    }

    #[test]
    fn cellar_formula_appends_keg_root() {
        let env = scratch_env();
        assert_eq!(
            resolve(&cli(&["zapbrew", "--cellar", "wget"]), &env),
            FastPath::Print(env.cellar.join("wget").into_string())
        );
    }

    #[test]
    fn cache_formula_is_refused() {
        let env = scratch_env();
        assert_eq!(
            resolve(&cli(&["zapbrew", "--cache", "wget"]), &env),
            FastPath::Refuse(CACHE_FORMULA.to_owned())
        );
    }

    #[test]
    fn repository_tap_resolves_canonical_path() {
        let env = scratch_env();
        assert_eq!(
            resolve(&cli(&["zapbrew", "--repository", "homebrew/core"]), &env),
            FastPath::Print(
                env.library
                    .join("Taps/Homebrew/homebrew-core")
                    .into_string()
            )
        );
    }

    #[test]
    fn repository_invalid_tap_is_refused() {
        let env = scratch_env();
        assert!(matches!(
            resolve(&cli(&["zapbrew", "--repository", "no-slash"]), &env),
            FastPath::Refuse(_)
        ));
    }

    #[test]
    fn multiple_path_queries_are_refused_once() {
        let env = scratch_env();
        assert_eq!(
            resolve(&cli(&["zapbrew", "--prefix", "--cellar"]), &env),
            FastPath::Refuse(MULTIPLE.to_owned())
        );
    }
}
