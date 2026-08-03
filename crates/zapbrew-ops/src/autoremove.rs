use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(test)]
use std::sync::Mutex;

use zapbrew_prefix::{Rack, Tab};

use crate::state::{InstalledState, scan_selected};
use crate::transaction::acquire_formula_locks;
use crate::{Ctx, OpError};

#[cfg(test)]
static AFTER_ENUMERATION: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Args {
    pub dry_run: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let locked_names = Rack::all(&ctx.env.cellar)?
        .into_iter()
        .map(|rack| rack.name().to_owned())
        .collect::<BTreeSet<_>>();
    let _locks = if args.dry_run {
        None
    } else {
        Some(acquire_formula_locks(&ctx.env, &locked_names)?)
    };
    #[cfg(test)]
    if let Some(hook) = AFTER_ENUMERATION
        .lock()
        .expect("autoremove test hook lock")
        .take()
    {
        hook();
    }
    let state = scan_selected(&ctx.env, &locked_names)?;
    let requested = strict_request_state(&state)?;
    let removable = fixpoint(&state, &requested);
    if removable.is_empty() {
        return Ok(());
    }

    let noun = if args.dry_run || removable.len() != 1 {
        "formulae"
    } else {
        "formula"
    };
    let verb = if args.dry_run {
        "Would autoremove"
    } else {
        "Autoremoving"
    };
    ctx.reporter
        .oh1(&format!("{verb} {} unneeded {noun}:", removable.len()));
    ctx.reporter.print(
        &removable
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    if args.dry_run {
        return Ok(());
    }
    crate::uninstall::remove_locked(ctx, &state, &removable, true)
}

fn strict_request_state(state: &InstalledState) -> Result<BTreeMap<String, bool>, OpError> {
    let mut requested = BTreeMap::new();
    for formula in state.iter() {
        let mut installed_on_request = false;
        for keg in formula.kegs() {
            let receipt = keg.path().join("INSTALL_RECEIPT.json");
            let content = fs::read_to_string(receipt.as_std_path()).map_err(|source| {
                OpError::io("read autoremove receipt", receipt.clone(), source)
            })?;
            let tab =
                serde_json::from_str::<Tab>(&content).map_err(|source| OpError::InvalidState {
                    reason: format!("invalid autoremove receipt {receipt}: {source}"),
                })?;
            installed_on_request |= tab.installed_on_request;
        }
        requested.insert(formula.name().name().to_owned(), installed_on_request);
    }
    Ok(requested)
}

fn fixpoint(state: &InstalledState, requested: &BTreeMap<String, bool>) -> BTreeSet<String> {
    let mut removable = BTreeSet::new();
    loop {
        let additions = state
            .iter()
            .filter(|formula| {
                let name = formula.name().name();
                !removable.contains(name) && !requested.get(name).copied().unwrap_or(true)
            })
            .filter(|formula| {
                state
                    .dependents_of(formula.name().name())
                    .into_iter()
                    .all(|dependent| removable.contains(dependent.name().name()))
            })
            .map(|formula| formula.name().name().to_owned())
            .collect::<Vec<_>>();
        if additions.is_empty() {
            return removable;
        }
        removable.extend(additions);
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
        CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Tab,
    };
    use zapbrew_types::{FormulaName, PkgVersion};

    use super::{AFTER_ENUMERATION, Args, run};
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

    fn keg(env: &Env, name: &str) -> Keg {
        let keg = Keg::new(
            &env.cellar,
            FormulaName::from_str(name).expect("formula name"),
            PkgVersion::from_str("1.0").expect("version"),
        )
        .expect("keg");
        std::fs::create_dir_all(keg.path()).expect("keg dir");
        Tab {
            installed_on_request: false,
            ..Tab::default()
        }
        .write(keg.receipt_path())
        .expect("tab");
        keg
    }

    #[tokio::test]
    async fn late_rack_is_excluded_from_locked_snapshot() {
        let temp = TempDir::new().expect("temp");
        let ctx = context(&temp);
        let original = keg(&ctx.env, "original");
        let late_env = ctx.env.clone();
        *AFTER_ENUMERATION.lock().expect("autoremove test hook lock") = Some(Box::new(move || {
            keg(&late_env, "late");
        }));

        run(&ctx, Args::default()).await.expect("autoremove");

        assert!(!original.path().exists());
        assert!(ctx.env.cellar.join("late/1.0").exists());
    }
}
