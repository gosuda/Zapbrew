use std::collections::BTreeSet;
use std::fs;
use std::str::FromStr;
#[cfg(test)]
use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_prefix::{Keg, Rack};
use zapbrew_types::FormulaName;

use crate::install::format_size;
use crate::state::{InstalledFormula, InstalledState, scan_selected};
use crate::transaction::{RemovalInput, RemovalKeg, acquire_formula_locks, remove_formulae};
use crate::{Ctx, OpError};

#[cfg(test)]
static AFTER_ENUMERATION: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

#[cfg(test)]
static LAST_SNAPSHOT: Mutex<Option<(BTreeSet<String>, BTreeSet<String>)>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub force: bool,
    pub ignore_dependencies: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.names.is_empty() {
        return Err(OpError::Refusal {
            message: "No formulae specified for uninstall.".to_owned(),
        });
    }
    let names = canonical_names(&args.names)?;
    let mut locked_names = Rack::all(&ctx.env.cellar)?
        .into_iter()
        .map(|rack| rack.name().to_owned())
        .collect::<BTreeSet<_>>();
    locked_names.extend(names.iter().cloned());
    let locks = acquire_formula_locks(&ctx.env, &locked_names)?;
    #[cfg(test)]
    if let Some(hook) = AFTER_ENUMERATION
        .lock()
        .expect("uninstall test hook lock")
        .take()
    {
        hook();
    }
    let state = scan_selected(&ctx.env, &locked_names)?;
    #[cfg(test)]
    {
        let scanned = state
            .iter()
            .map(|formula| formula.name().name().to_owned())
            .collect::<BTreeSet<_>>();
        *LAST_SNAPSHOT.lock().expect("uninstall snapshot lock") =
            Some((locked_names.clone(), scanned));
    }
    preflight(&ctx.env.cellar, &state, &names, &args)?;
    remove_locked(ctx, &state, &names, args.force)?;
    drop(locks);
    if !ctx.env.no_autoremove {
        crate::autoremove::run(ctx, crate::autoremove::Args::default()).await?;
    }
    Ok(())
}

pub(crate) fn remove_locked(
    ctx: &Ctx,
    state: &InstalledState,
    names: &BTreeSet<String>,
    force: bool,
) -> Result<(), OpError> {
    let mut transactions = Vec::with_capacity(names.len());
    let mut remaining = Vec::new();
    let mut leftovers = Vec::new();

    for name in names {
        let installed = state.formula(name).ok_or_else(|| OpError::Refusal {
            message: format!("No such keg: {}/{name}", ctx.env.cellar),
        })?;
        let targets = targets(installed, force);
        for keg in &targets {
            ctx.reporter.print(&format!(
                "Uninstalling {}... ({})",
                keg.path(),
                format_size(keg.size())
            ));
        }
        let selected = targets
            .iter()
            .map(|keg| {
                Ok(RemovalKeg {
                    keg: Keg::new(
                        &ctx.env.cellar,
                        installed.name().clone(),
                        keg.version().clone(),
                    )?,
                    linked: keg.is_linked(),
                    optlinked: keg.is_optlinked(),
                })
            })
            .collect::<Result<Vec<_>, OpError>>()?;
        let remove_rack = selected.len() == installed.kegs().len();
        if !remove_rack {
            remaining.push((
                name.clone(),
                installed
                    .kegs()
                    .iter()
                    .filter(|keg| {
                        !targets
                            .iter()
                            .any(|target| target.version() == keg.version())
                    })
                    .map(|keg| keg.version().to_string())
                    .collect::<Vec<_>>(),
            ));
        }
        leftovers.push((name.clone(), configuration_paths(ctx, name)?));
        transactions.push(RemovalInput {
            name: installed.name().clone(),
            targets: selected,
            remove_rack,
        });
    }

    remove_formulae(ctx, transactions)?;

    for (name, versions) in remaining {
        let verb = if versions.len() == 1 { "is" } else { "are" };
        ctx.reporter.print(&format!(
            "{name} {} {verb} still installed.\nTo remove all versions, run:\n  brew uninstall --force {name}",
            to_sentence(&versions)
        ));
    }
    for (name, paths) in leftovers {
        if !paths.is_empty() {
            ctx.reporter.opoo(&format!(
                "The following {name} configuration files have not been removed!\nIf desired, remove them manually with rm -rf:\n  {}",
                paths
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
        }
    }
    Ok(())
}

fn canonical_names(requested: &[String]) -> Result<BTreeSet<String>, OpError> {
    requested
        .iter()
        .map(|name| {
            FormulaName::from_str(name)
                .map(|name| name.name().to_owned())
                .map_err(|source| OpError::InvalidState {
                    reason: format!("formula name {name} is invalid: {source}"),
                })
        })
        .collect()
}

fn preflight(
    cellar: &Utf8Path,
    state: &InstalledState,
    names: &BTreeSet<String>,
    args: &Args,
) -> Result<(), OpError> {
    for name in names {
        if !state.contains(name) {
            return Err(OpError::Refusal {
                message: format!("No such keg: {cellar}/{name}"),
            });
        }
    }

    if !args.ignore_dependencies {
        let mut required = Vec::new();
        let mut dependents = BTreeSet::new();
        for name in names {
            let installed = state.formula(name).ok_or_else(|| OpError::InvalidState {
                reason: format!("installed formula {name} disappeared during preflight"),
            })?;
            let outside = state
                .dependents_of(name)
                .into_iter()
                .filter(|formula| !names.contains(formula.name().name()))
                .collect::<Vec<_>>();
            if !outside.is_empty() {
                required.extend(
                    targets(installed, args.force)
                        .into_iter()
                        .map(|keg| keg.path().to_string()),
                );
                dependents.extend(
                    outside
                        .into_iter()
                        .map(|formula| formula.name().name().to_owned()),
                );
            }
        }
        if !required.is_empty() {
            let required_verb = if required.len() == 1 {
                "it is"
            } else {
                "they are"
            };
            let dependent_verb = if dependents.len() == 1 { "is" } else { "are" };
            return Err(OpError::Refusal {
                message: format!(
                    "Refusing to uninstall {}\nbecause {required_verb} required by {}, which {dependent_verb} currently installed.\nYou can override this and force removal with:\n  brew uninstall --ignore-dependencies {}",
                    to_sentence(&required),
                    to_sentence(&dependents.into_iter().collect::<Vec<_>>()),
                    args.names.join(" ")
                ),
            });
        }
    }

    for name in names {
        let installed = state.formula(name).ok_or_else(|| OpError::InvalidState {
            reason: format!("installed formula {name} disappeared during pin preflight"),
        })?;
        if !args.force && installed.pinned().is_some() {
            return Err(OpError::Refusal {
                message: format!("{name} is pinned. You must unpin it to uninstall."),
            });
        }
    }
    Ok(())
}

fn targets(installed: &InstalledFormula, force: bool) -> Vec<&crate::state::InstalledKeg> {
    if force {
        return installed.kegs().iter().collect();
    }
    installed
        .linked()
        .or_else(|| installed.optlinked())
        .or_else(|| installed.latest())
        .into_iter()
        .collect()
}

fn configuration_paths(ctx: &Ctx, name: &str) -> Result<Vec<Utf8PathBuf>, OpError> {
    let root = ctx.env.prefix.join("etc").join(name);
    let metadata = match fs::symlink_metadata(root.as_std_path()) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(OpError::io("inspect", root, source)),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(vec![root]);
    }
    let mut paths = Vec::new();
    collect_paths(&root, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_paths(path: &Utf8Path, paths: &mut Vec<Utf8PathBuf>) -> Result<(), OpError> {
    let entries = fs::read_dir(path.as_std_path())
        .map_err(|source| OpError::io("read configuration directory", path, source))?;
    for entry in entries {
        let entry =
            entry.map_err(|source| OpError::io("read configuration entry", path, source))?;
        let child =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("configuration path is not UTF-8: {}", path.display()),
            })?;
        paths.push(child.clone());
        let metadata = fs::symlink_metadata(child.as_std_path())
            .map_err(|source| OpError::io("inspect", child.clone(), source))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_paths(&child, paths)?;
        }
    }
    Ok(())
}

fn to_sentence(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [left, right] => format!("{left} and {right}"),
        _ => {
            let Some((last, rest)) = items.split_last() else {
                return String::new();
            };
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;
    use std::str::FromStr;
    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use zapbrew_api::{CaskCatalog, Catalog};
    use zapbrew_prefix::{
        CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, RuntimeDependency,
        Source, SourceVersions, Tab,
    };
    use zapbrew_types::{FormulaName, PkgVersion};

    use super::{AFTER_ENUMERATION, Args, LAST_SNAPSHOT, run};
    use crate::{Ctx, Reporter};

    struct PanicRunner;

    impl CommandRunner for PanicRunner {
        fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
            panic!("host command must not run: {:?}", spec.program())
        }
    }

    struct NullReporter;

    impl Reporter for NullReporter {
        fn ohai(&self, _: &str) {}
        fn oh1(&self, _: &str) {}
        fn opoo(&self, _: &str) {}
        fn onoe(&self, _: &str) {}
        fn print(&self, _: &str) {}
        fn eprint(&self, _: &str) {}
    }

    fn context(temp: &TempDir) -> Ctx {
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
        let env = Env::detect_from(
            &EnvDetectInput {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                home: root.join("home"),
                xdg_cache_home: None,
                vars: HashMap::from([
                    (
                        "HOMEBREW_PREFIX".to_owned(),
                        root.join("prefix").to_string(),
                    ),
                    ("HOMEBREW_CACHE".to_owned(), root.join("cache").to_string()),
                    ("HOMEBREW_TEMP".to_owned(), root.join("temp").to_string()),
                    ("HOMEBREW_NO_AUTOREMOVE".to_owned(), "1".to_owned()),
                ]),
                available_parallelism: 2,
            },
            &PanicRunner,
        )
        .expect("scratch env");
        Ctx {
            catalog: Arc::new(
                Catalog::from_payload(b"[]", &env.bottle_tag).expect("formula catalog"),
            ),
            casks: Arc::new(
                CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("cask catalog"),
            ),
            env,
            http: reqwest::Client::new(),
            commands: Arc::new(PanicRunner),
            reporter: Arc::new(NullReporter),
        }
    }

    fn keg(env: &Env, name: &str, dependencies: &[&str]) -> Keg {
        let keg = Keg::new(
            &env.cellar,
            FormulaName::from_str(name).expect("formula name"),
            PkgVersion::from_str("1.0").expect("version"),
        )
        .expect("keg");
        std::fs::create_dir_all(keg.path()).expect("keg dir");
        Tab {
            installed_on_request: true,
            runtime_dependencies: Some(
                dependencies
                    .iter()
                    .map(|dependency| RuntimeDependency {
                        full_name: (*dependency).to_owned(),
                        version: "1.0".to_owned(),
                        revision: 0,
                        bottle_rebuild: None,
                        pkg_version: "1.0".to_owned(),
                        declared_directly: true,
                        compatibility_version: None,
                    })
                    .collect(),
            ),
            source: Source {
                versions: SourceVersions {
                    stable: Some(keg.version().to_string()),
                    version_scheme: 0,
                    ..SourceVersions::default()
                },
                ..Source::default()
            },
            ..Tab::default()
        }
        .write(keg.receipt_path())
        .expect("tab");
        keg
    }

    #[tokio::test]
    async fn late_rack_is_excluded_and_scanned_names_are_locked() {
        let temp = TempDir::new().expect("temp");
        let ctx = context(&temp);
        let dependency = keg(&ctx.env, "dep", &[]);
        let late_env = ctx.env.clone();
        *AFTER_ENUMERATION.lock().expect("uninstall test hook lock") = Some(Box::new(move || {
            keg(&late_env, "late", &["dep"]);
        }));
        *LAST_SNAPSHOT.lock().expect("uninstall snapshot lock") = None;

        run(
            &ctx,
            Args {
                names: vec!["dep".to_owned()],
                ..Args::default()
            },
        )
        .await
        .expect("uninstall ignores late dependent");

        assert!(!dependency.path().exists());
        assert!(ctx.env.cellar.join("late/1.0").exists());
        let (locked, scanned) = LAST_SNAPSHOT
            .lock()
            .expect("uninstall snapshot lock")
            .take()
            .expect("snapshot recorded");
        assert!(!scanned.contains("late"));
        assert!(!locked.contains("late"));
        assert!(scanned.is_subset(&locked));
        assert!(locked.contains("dep"));
        assert!(scanned.contains("dep"));
    }
}
