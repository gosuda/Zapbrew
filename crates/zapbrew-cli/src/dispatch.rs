//! Async command dispatch.
//!
//! [`plan`] is a pure, exhaustive conversion of the typed clap surface into one
//! [`OpKind`] per operation plus the catalog classification the command needs.
//! [`run`] wires the runtime: it builds one HTTP client, loads only the
//! required catalog(s), assembles a [`Ctx`], and runs the operation under a
//! `tokio::select!` against `ctrl_c()`. Success exits 0, an [`OpError`] is
//! reported once and exits 1, and SIGINT exits 130 without cancelling cleanup so
//! a partial `.incomplete` download survives.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use crate::cli::{Cli, Commands, GlobalArgs, ServicesCommand, ShimCommand};
use crate::output;
use camino::Utf8PathBuf;
use zapbrew_api::{CaskCatalog, Catalog, Resolution};
use zapbrew_ops::{
    Ctx, OpError, Reporter, autoremove, cask, cleanup, config, deps, desc, doctor, fetch, info,
    install, leaves, link, list, outdated, pin, postinstall, reinstall, search, services, shellenv,
    shim, tap, tap_info, uninstall, unlink, unpin, untap, update, upgrade, uses,
};
use zapbrew_prefix::{Env, SystemCommandRunner};

/// One converted operation, ready to run. Each variant owns the exact
/// `zapbrew-ops` `Args` produced from the parsed clap command.
#[derive(Debug, PartialEq, Eq)]
pub enum OpKind {
    Install(install::Args),
    CaskInstall(cask::install::Args),
    Reinstall(reinstall::Args),
    Uninstall(uninstall::Args),
    CaskUninstall(cask::uninstall::Args),
    Upgrade(upgrade::Args),
    Outdated(outdated::Args),
    List(list::Args),
    CaskList(cask::list::Args),
    Info(info::Args),
    Deps(deps::Args),
    Uses(uses::Args),
    Leaves(leaves::Args),
    Autoremove(autoremove::Args),
    Pin(pin::Args),
    Unpin(unpin::Args),
    Link(link::Args),
    Unlink(unlink::Args),
    Fetch(fetch::Args),
    Cleanup(cleanup::Args),
    Search(search::Args),
    Desc(desc::Args),
    Postinstall(postinstall::Args),
    Config(config::Args),
    Shellenv(shellenv::Args),
    Tap(tap::Args),
    Untap(untap::Args),
    TapInfo(tap_info::Args),
    Doctor(doctor::Args),
    Services(services::Args),
    Update(update::Args),
    Shim(shim::Args),
}

/// A planned operation plus the catalogs it consults.
///
/// `needs_formula`/`needs_cask` decide real-vs-empty catalog construction so a
/// command never loads a catalog it does not read.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub needs_formula: bool,
    pub needs_cask: bool,
    pub kind: OpKind,
}

/// Convert one parsed command into its operation and catalog classification.
///
/// `width` is the resolved output width (zero forces one item per line). The
/// fallible steps are `--appdir` without `--cask`, a non-UTF-8 `--appdir`, and a
/// formula-only install flag combined with `--cask`, all returned as typed refusals.
pub fn plan(command: Commands, globals: &GlobalArgs, width: usize) -> Result<Plan, OpError> {
    let plan = match command {
        Commands::Install(args) => {
            if args.cask {
                if let Some(flag) = unsupported_cask_flag(&args) {
                    return Err(OpError::Refusal {
                        message: format!(
                            "zapbrew cannot honor {flag} with --cask: the cask install path does not support it. Use brew."
                        ),
                    });
                }
                Plan {
                    needs_formula: false,
                    needs_cask: true,
                    kind: OpKind::CaskInstall(cask::install::Args {
                        tokens: args.names,
                        appdir: appdir(args.appdir)?,
                        force: args.force,
                    }),
                }
            } else {
                if args.appdir.is_some() {
                    return Err(OpError::Refusal {
                        message: "zapbrew cannot honor --appdir without --cask: \
                                  the formula install path does not support it. Use brew."
                            .to_owned(),
                    });
                }
                Plan {
                    needs_formula: true,
                    needs_cask: false,
                    kind: OpKind::Install(install::Args {
                        names: args.names,
                        only_dependencies: args.only_dependencies,
                        force: args.force,
                        dry_run: args.dry_run,
                        build_from_source: args.build_from_source,
                        head: args.head,
                        interactive: args.interactive,
                        include_test: args.include_test,
                    }),
                }
            }
        }
        Commands::Reinstall(args) => {
            if args.formula && args.appdir.is_some() {
                return Err(OpError::Refusal {
                    message: "zapbrew cannot honor --appdir with --formula: \
                             the formula reinstall path does not support it. Use brew."
                        .to_owned(),
                });
            }
            let (needs_formula, needs_cask) = match (args.formula, args.cask) {
                (true, false) => (true, false),
                (false, true) => (false, true),
                _ => (true, true),
            };
            Plan {
                needs_formula,
                needs_cask,
                kind: OpKind::Reinstall(reinstall::Args {
                    names: args.names,
                    formula: args.formula,
                    cask: args.cask,
                    appdir: appdir(args.appdir)?,
                }),
            }
        }
        Commands::Uninstall(args) => {
            if args.cask {
                if let Some(flag) = unsupported_cask_uninstall_flag(&args) {
                    return Err(OpError::Refusal {
                        message: format!(
                            "zapbrew cannot honor {flag} with --cask: the cask uninstall path does not support it. Use brew."
                        ),
                    });
                }
                Plan {
                    needs_formula: false,
                    needs_cask: true,
                    kind: OpKind::CaskUninstall(cask::uninstall::Args {
                        tokens: args.names,
                        zap: args.zap,
                    }),
                }
            } else {
                Plan {
                    needs_formula: false,
                    needs_cask: false,
                    kind: OpKind::Uninstall(uninstall::Args {
                        names: args.names,
                        force: args.force,
                        ignore_dependencies: args.ignore_dependencies,
                    }),
                }
            }
        }
        Commands::Upgrade(args) => {
            let mode = if args.cask {
                upgrade::Mode::Cask
            } else if args.formula {
                upgrade::Mode::Formula
            } else {
                upgrade::Mode::Auto
            };
            Plan {
                needs_formula: mode != upgrade::Mode::Cask,
                needs_cask: mode != upgrade::Mode::Formula,
                kind: OpKind::Upgrade(upgrade::Args {
                    names: args.names,
                    dry_run: args.dry_run,
                    mode,
                    appdir: appdir(args.appdir)?,
                    greedy: args.greedy,
                    greedy_latest: args.greedy_latest,
                    greedy_auto_updates: args.greedy_auto_updates,
                }),
            }
        }
        Commands::Outdated(args) => Plan {
            needs_formula: true,
            needs_cask: true,
            kind: OpKind::Outdated(outdated::Args {
                names: args.names,
                verbose: globals.verbose,
                json_v2: args.json.is_some(),
                greedy: args.greedy,
                greedy_latest: args.greedy_latest,
                greedy_auto_updates: args.greedy_auto_updates,
            }),
        },
        Commands::List(args) => {
            if args.cask {
                Plan {
                    needs_formula: false,
                    needs_cask: true,
                    kind: OpKind::CaskList(cask::list::Args {
                        tokens: args.names,
                        versions: args.versions,
                        one_per_line: args.oneline,
                        width,
                    }),
                }
            } else {
                Plan {
                    needs_formula: !args.names.is_empty(),
                    needs_cask: false,
                    kind: OpKind::List(list::Args {
                        names: args.names,
                        versions: args.versions,
                        oneline: args.oneline,
                        width,
                    }),
                }
            }
        }
        Commands::Info(args) => {
            let needs_catalog = !args.names.is_empty() || args.json.is_some();
            Plan {
                needs_formula: needs_catalog,
                needs_cask: needs_catalog,
                kind: OpKind::Info(info::Args {
                    names: args.names,
                    json_v2: args.json.is_some(),
                }),
            }
        }
        Commands::Deps(args) => Plan {
            needs_formula: true,
            needs_cask: false,
            kind: OpKind::Deps(deps::Args {
                names: args.names,
                tree: args.tree,
                union: args.union,
                include_build: args.include_build,
                include_test: args.include_test,
                include_optional: args.include_optional,
                skip_recommended: args.skip_recommended,
            }),
        },
        Commands::Uses(args) => Plan {
            needs_formula: true,
            needs_cask: true,
            kind: OpKind::Uses(uses::Args {
                names: args.names,
                recursive: args.recursive,
                installed: args.installed,
                include_build: args.include_build,
                include_test: args.include_test,
                include_optional: args.include_optional,
                skip_recommended: args.skip_recommended,
                width,
            }),
        },
        Commands::Leaves(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Leaves(leaves::Args {
                filter: leaves_filter(args.installed_on_request, args.installed_as_dependency),
            }),
        },
        Commands::Autoremove(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Autoremove(autoremove::Args {
                dry_run: args.dry_run,
            }),
        },
        Commands::Pin(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Pin(pin::Args { names: args.names }),
        },
        Commands::Unpin(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Unpin(unpin::Args { names: args.names }),
        },
        Commands::Link(args) => Plan {
            needs_formula: true,
            needs_cask: false,
            kind: OpKind::Link(link::Args {
                names: args.names,
                overwrite: args.overwrite,
                dry_run: args.dry_run,
                force: args.force,
            }),
        },
        Commands::Unlink(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Unlink(unlink::Args {
                names: args.names,
                dry_run: args.dry_run,
            }),
        },
        Commands::Fetch(args) => {
            let (needs_formula, needs_cask, mode) = match (args.formula, args.cask) {
                (true, false) => (true, false, fetch::Mode::FormulaOnly),
                (false, true) => (false, true, fetch::Mode::CaskOnly),
                _ => (true, true, fetch::Mode::Auto),
            };
            Plan {
                needs_formula,
                needs_cask,
                kind: OpKind::Fetch(fetch::Args {
                    names: args.names,
                    deps: args.deps,
                    mode,
                }),
            }
        }
        Commands::Cleanup(args) => Plan {
            needs_formula: true,
            needs_cask: false,
            kind: OpKind::Cleanup(cleanup::Args {
                names: args.names,
                dry_run: args.dry_run,
                scrub: args.scrub,
                prune: args.prune,
                prune_prefix: args.prune_prefix,
            }),
        },
        Commands::Search(args) => Plan {
            needs_formula: !args.cask,
            needs_cask: !args.formula,
            kind: OpKind::Search(search::Args {
                query: args.query,
                desc: args.desc,
                formula_only: args.formula,
                cask_only: args.cask,
                width,
            }),
        },
        Commands::Desc(args) => Plan {
            needs_formula: !args.cask,
            needs_cask: !args.formula,
            kind: OpKind::Desc(desc::Args {
                names: args.names,
                search: args.search,
                search_name: args.search_name,
                search_description: args.search_description,
                formula: args.formula,
                cask: args.cask,
            }),
        },
        Commands::Postinstall(args) => Plan {
            needs_formula: true,
            needs_cask: false,
            kind: OpKind::Postinstall(postinstall::Args { names: args.names }),
        },
        Commands::Config => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Config(config::Args),
        },
        Commands::Shellenv(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Shellenv(shellenv::Args { shell: args.shell }),
        },
        Commands::Tap(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Tap(tap::Args {
                name: args.name,
                url: args.url,
                force: args.force,
            }),
        },
        Commands::Untap(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Untap(untap::Args {
                names: args.names,
                force: args.force,
            }),
        },
        Commands::TapInfo(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::TapInfo(tap_info::Args {
                names: args.names,
                installed: args.installed,
                json: args.json,
            }),
        },
        Commands::Doctor => Plan {
            needs_formula: true,
            needs_cask: false,
            kind: OpKind::Doctor(doctor::Args),
        },
        Commands::Services(args) => Plan {
            needs_formula: true,
            needs_cask: false,
            kind: OpKind::Services(services_args(args.command.unwrap_or(ServicesCommand::List))),
        },
        Commands::Update => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Update(update::Args::default()),
        },
        Commands::Shim(args) => Plan {
            needs_formula: false,
            needs_cask: false,
            kind: OpKind::Shim(shim::Args {
                action: shim_action(args.command),
            }),
        },
        // `main` intercepts completion generation before the runtime ever runs,
        // so reaching dispatch means the interception was lost. Refuse with a
        // typed error rather than silently doing nothing.
        Commands::Completions(_) => {
            return Err(OpError::InvalidState {
                reason: "completions must be intercepted by main before dispatch".to_owned(),
            });
        }
    };
    Ok(plan)
}

/// Map the two mutually exclusive leaves flags to the ops filter.
fn leaves_filter(on_request: bool, as_dependency: bool) -> leaves::Filter {
    if on_request {
        leaves::Filter::OnRequest
    } else if as_dependency {
        leaves::Filter::AsDependency
    } else {
        leaves::Filter::All
    }
}

/// Map the services subcommand to the ops action plus its name list.
fn services_args(command: ServicesCommand) -> services::Args {
    let (action, names) = match command {
        ServicesCommand::List => (services::ServiceAction::List, Vec::new()),
        ServicesCommand::Start { names } => (services::ServiceAction::Start, names),
        ServicesCommand::Stop { names } => (services::ServiceAction::Stop, names),
        ServicesCommand::Restart { names } => (services::ServiceAction::Restart, names),
        ServicesCommand::Run { names } => (services::ServiceAction::Run, names),
        ServicesCommand::Info { names } => (services::ServiceAction::Info, names),
        ServicesCommand::Kill { names } => (services::ServiceAction::Kill, names),
        ServicesCommand::Cleanup => (services::ServiceAction::Cleanup, Vec::new()),
    };
    services::Args { action, names }
}

/// Map the shim subcommand to its ops action.
fn shim_action(command: ShimCommand) -> shim::ShimAction {
    match command {
        ShimCommand::Install => shim::ShimAction::Install,
        ShimCommand::Remove => shim::ShimAction::Remove,
    }
}

/// Name the first install flag that `cask::install::Args` cannot carry.
///
/// The cask argument surface is `tokens`, `appdir` and `force`. Every other
/// `InstallArgs` field would be dropped on the way through, so accepting one
/// and installing anyway would silently discard an explicit request — and for
/// `--dry-run` and `--only-dependencies` it would mutate the Caskroom and the
/// application directory that the flag asked it to leave alone. Refuse instead.
/// Order is the flag order in `InstallArgs`, so the message is stable.
fn unsupported_cask_flag(args: &crate::cli::InstallArgs) -> Option<&'static str> {
    [
        (args.only_dependencies, "--only-dependencies"),
        (args.dry_run, "--dry-run"),
        (args.build_from_source, "--build-from-source"),
        (args.head, "--HEAD"),
        (args.interactive, "--interactive"),
        (args.include_test, "--include-test"),
    ]
    .into_iter()
    .find_map(|(present, flag)| present.then_some(flag))
}

/// Cask uninstall can only honor `--zap` and the token list. `--force` and
/// `--ignore-dependencies` have Homebrew semantics that the current cask
/// uninstall path does not model (best-effort removal of uninstalled or partly
/// damaged casks, and indirect cask-dependent checks). Refuse instead.
fn unsupported_cask_uninstall_flag(args: &crate::cli::UninstallArgs) -> Option<&'static str> {
    [
        (args.force, "--force"),
        (args.ignore_dependencies, "--ignore-dependencies"),
    ]
    .into_iter()
    .find_map(|(present, flag)| present.then_some(flag))
}

/// Convert an optional cask `--appdir` to a UTF-8 path, refusing non-UTF-8.
fn appdir(appdir: Option<PathBuf>) -> Result<Option<Utf8PathBuf>, OpError> {
    appdir
        .map(Utf8PathBuf::from_path_buf)
        .transpose()
        .map_err(|path| OpError::Refusal {
            message: format!("--appdir path is not valid UTF-8: {}", path.display()),
        })
}

/// Drive one command to completion under the runtime.
pub async fn run(cli: Cli, env: Env, reporter: Arc<dyn Reporter>) -> ExitCode {
    let Some(command) = cli.command else {
        // Unreachable in normal flow: `main` prints help for a command-less
        // invocation before building the runtime. Succeed defensively.
        return output::success();
    };

    let plan = match plan(command, &cli.globals, stdout_width()) {
        Ok(plan) => plan,
        Err(err) => {
            output::report_error(reporter.as_ref(), &err);
            return output::exit_code(&err);
        }
    };

    let http = reqwest::Client::new();
    let (catalog, casks) =
        match load_catalogs(plan.needs_formula, plan.needs_cask, &env, &http).await {
            Ok(catalogs) => catalogs,
            Err(err) => {
                output::report_error(reporter.as_ref(), &err);
                return output::exit_code(&err);
            }
        };

    let ctx = Ctx {
        env,
        http,
        catalog,
        casks,
        commands: Arc::new(SystemCommandRunner),
        reporter,
    };

    tokio::select! {
        result = execute(plan.kind, &ctx) => finish(result, &ctx),
        _ = tokio::signal::ctrl_c() => ExitCode::from(output::SIGINT_EXIT_CODE),
    }
}

/// Load the required catalogs, concurrently when both are needed, using empty
/// payloads for domains the command does not consult.
async fn load_catalogs(
    needs_formula: bool,
    needs_cask: bool,
    env: &Env,
    http: &reqwest::Client,
) -> Result<(Arc<Catalog>, Arc<CaskCatalog>), OpError> {
    let formula = async {
        if needs_formula {
            Ok::<Catalog, OpError>(Catalog::load(env, http).await?)
        } else {
            Ok(Catalog::from_payload(b"[]", &env.bottle_tag)?)
        }
    };
    let cask = async {
        if needs_cask {
            Ok::<CaskCatalog, OpError>(CaskCatalog::load(env, http).await?)
        } else {
            Ok(CaskCatalog::from_payload(b"[]", &env.bottle_tag)?)
        }
    };
    let (formula, cask) = tokio::join!(formula, cask);
    Ok((Arc::new(formula?), Arc::new(cask?)))
}

/// Run one converted operation. Shim is synchronous; every other verb awaits.
async fn execute(kind: OpKind, ctx: &Ctx) -> Result<(), OpError> {
    match kind {
        OpKind::Install(args) => install::run(ctx, args).await,
        OpKind::CaskInstall(args) => cask::install::run(ctx, args).await,
        OpKind::Reinstall(args) => reinstall::run(ctx, args).await,
        OpKind::Uninstall(args) => uninstall::run(ctx, args).await,
        OpKind::CaskUninstall(args) => cask::uninstall::run(ctx, args).await,
        OpKind::Upgrade(args) => upgrade::run(ctx, args).await,
        OpKind::Outdated(args) => outdated::run(ctx, args).await,
        OpKind::List(args) => list::run(ctx, args).await,
        OpKind::CaskList(args) => cask::list::run(ctx, args).await,
        OpKind::Info(args) => info::run(ctx, args).await,
        OpKind::Deps(args) => deps::run(ctx, args).await,
        OpKind::Uses(args) => uses::run(ctx, args).await,
        OpKind::Leaves(args) => leaves::run(ctx, args).await,
        OpKind::Autoremove(args) => autoremove::run(ctx, args).await,
        OpKind::Pin(args) => pin::run(ctx, args).await,
        OpKind::Unpin(args) => unpin::run(ctx, args).await,
        OpKind::Link(args) => link::run(ctx, args).await,
        OpKind::Unlink(args) => unlink::run(ctx, args).await,
        OpKind::Fetch(args) => fetch::run(ctx, args).await,
        OpKind::Cleanup(args) => cleanup::run(ctx, args).await,
        OpKind::Search(args) => search::run(ctx, args).await,
        OpKind::Desc(args) => desc::run(ctx, args).await,
        OpKind::Postinstall(args) => postinstall::run(ctx, args).await,
        OpKind::Config(args) => config::run(ctx, args).await,
        OpKind::Shellenv(args) => shellenv::run(ctx, args).await,
        OpKind::Tap(args) => tap::run(ctx, args).await,
        OpKind::Untap(args) => untap::run(ctx, args).await,
        OpKind::TapInfo(args) => tap_info::run(ctx, args).await,
        OpKind::Doctor(args) => doctor::run(ctx, args).await,
        OpKind::Services(args) => services::run(ctx, args).await,
        OpKind::Update(args) => update::run(ctx, args).await,
        OpKind::Shim(args) => shim::run(ctx, args),
    }
}

/// Map an operation result to an exit code, reporting an error once.
fn finish(result: Result<(), OpError>, ctx: &Ctx) -> ExitCode {
    match result {
        Ok(()) => output::success(),
        Err(err) => {
            report(ctx, &err);
            output::exit_code(&err)
        }
    }
}

/// Report an error once, appending a catalog-computed did-you-mean suffix for a
/// missing formula. Every other error renders through the shared owner.
fn report(ctx: &Ctx, err: &OpError) {
    if matches!(err, OpError::DoctorProblemsFound) {
        return;
    }
    if let OpError::MissingFormula { name } = err
        && let Resolution::Missing { did_you_mean } = ctx.catalog.resolve(name)
        && !did_you_mean.is_empty()
    {
        ctx.reporter
            .onoe(&format!("{err}{}", suggestion_suffix(&did_you_mean)));
        return;
    }
    output::report_error(ctx.reporter.as_ref(), err);
}

/// Build the ` Did you mean a?` / ` Did you mean a or b?` suffix.
fn suggestion_suffix(names: &[String]) -> String {
    match names {
        [only] => format!(" Did you mean {only}?"),
        [first, second, ..] => format!(" Did you mean {first} or {second}?"),
        [] => String::new(),
    }
}

/// Terminal width for column layout; zero (unknown) forces one item per line.
fn stdout_width() -> usize {
    terminal_size::terminal_size().map_or(0, |(width, _)| usize::from(width.0))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{OpKind, Plan, appdir, plan, suggestion_suffix};
    use crate::cli::Cli;

    const WIDTH: usize = 80;

    fn planned(args: &[&str]) -> Plan {
        let cli = Cli::parse_from(args);
        plan(cli.command.expect("command"), &cli.globals, WIDTH).expect("plan")
    }

    fn classify(args: &[&str]) -> (bool, bool) {
        let plan = planned(args);
        (plan.needs_formula, plan.needs_cask)
    }

    #[test]
    fn install_formula_maps_every_field() {
        let plan = planned(&[
            "zapbrew",
            "install",
            "wget",
            "curl",
            "--only-dependencies",
            "--force",
            "--dry-run",
            "--build-from-source",
            "--HEAD",
            "--interactive",
        ]);
        assert_eq!(
            plan.kind,
            OpKind::Install(zapbrew_ops::install::Args {
                names: vec!["wget".to_owned(), "curl".to_owned()],
                only_dependencies: true,
                force: true,
                dry_run: true,
                build_from_source: true,
                head: true,
                interactive: true,
                include_test: false,
            })
        );
        assert_eq!((plan.needs_formula, plan.needs_cask), (true, false));
    }

    #[test]
    fn install_cask_routes_to_cask_and_converts_appdir() {
        let plan = planned(&[
            "zapbrew", "install", "--cask", "firefox", "--force", "--appdir", "/Apps",
        ]);
        assert_eq!(
            plan.kind,
            OpKind::CaskInstall(zapbrew_ops::cask::install::Args {
                tokens: vec!["firefox".to_owned()],
                appdir: Some("/Apps".into()),
                force: true,
            })
        );
        assert_eq!((plan.needs_formula, plan.needs_cask), (false, true));
    }

    #[test]
    fn uninstall_routes_by_cask_flag() {
        assert_eq!(
            planned(&[
                "zapbrew",
                "uninstall",
                "wget",
                "--force",
                "--ignore-dependencies"
            ])
            .kind,
            OpKind::Uninstall(zapbrew_ops::uninstall::Args {
                names: vec!["wget".to_owned()],
                force: true,
                ignore_dependencies: true,
            })
        );
        assert_eq!(
            planned(&["zapbrew", "uninstall", "--cask", "firefox", "--zap"]).kind,
            OpKind::CaskUninstall(zapbrew_ops::cask::uninstall::Args {
                tokens: vec!["firefox".to_owned()],
                zap: true,
            })
        );
    }

    #[test]
    fn uninstall_cask_refuses_unsupported_flags() {
        for flag in ["--force", "--ignore-dependencies"] {
            let cli = Cli::parse_from(["zapbrew", "uninstall", "--cask", "firefox", flag]);
            let err = plan(cli.command.expect("command"), &cli.globals, WIDTH).expect_err(flag);
            match err {
                zapbrew_ops::OpError::Refusal { message } => assert!(
                    message.contains(flag) && message.contains("--cask"),
                    "refusal must name {flag}: {message}"
                ),
                other => panic!("expected a refusal for {flag}, got {other:?}"),
            }
        }
    }

    #[test]
    fn list_routes_and_forwards_width() {
        assert_eq!(
            planned(&["zapbrew", "list", "wget", "--versions", "-1"]).kind,
            OpKind::List(zapbrew_ops::list::Args {
                names: vec!["wget".to_owned()],
                versions: true,
                oneline: true,
                width: WIDTH,
            })
        );
        assert_eq!(
            planned(&["zapbrew", "list", "--cask", "-1"]).kind,
            OpKind::CaskList(zapbrew_ops::cask::list::Args {
                tokens: Vec::new(),
                versions: false,
                one_per_line: true,
                width: WIDTH,
            })
        );
    }

    #[test]
    fn outdated_sources_verbose_and_json() {
        assert_eq!(
            planned(&[
                "zapbrew",
                "-v",
                "outdated",
                "--json=v2",
                "--greedy",
                "--greedy-latest",
                "--greedy-auto-updates",
            ])
            .kind,
            OpKind::Outdated(zapbrew_ops::outdated::Args {
                names: Vec::new(),
                verbose: true,
                json_v2: true,
                greedy: true,
                greedy_latest: true,
                greedy_auto_updates: true,
            })
        );
        assert_eq!(
            planned(&["zapbrew", "outdated"]).kind,
            OpKind::Outdated(zapbrew_ops::outdated::Args {
                names: Vec::new(),
                verbose: false,
                json_v2: false,
                greedy: false,
                greedy_latest: false,
                greedy_auto_updates: false,
            })
        );
    }

    #[test]
    fn info_maps_json_flag() {
        assert_eq!(
            planned(&["zapbrew", "info", "wget", "--json=v2"]).kind,
            OpKind::Info(zapbrew_ops::info::Args {
                names: vec!["wget".to_owned()],
                json_v2: true,
            })
        );
    }

    #[test]
    fn info_no_name_text_is_offline_and_named_or_json_loads_catalogs() {
        // Bare text info scans the Cellar without catalogs.
        assert_eq!(
            planned(&["zapbrew", "info"]),
            Plan {
                needs_formula: false,
                needs_cask: false,
                kind: OpKind::Info(zapbrew_ops::info::Args {
                    names: Vec::new(),
                    json_v2: false,
                }),
            }
        );

        // Any names or --json forces catalog load.
        assert_eq!(
            planned(&["zapbrew", "info", "wget"]),
            Plan {
                needs_formula: true,
                needs_cask: true,
                kind: OpKind::Info(zapbrew_ops::info::Args {
                    names: vec!["wget".to_owned()],
                    json_v2: false,
                }),
            }
        );
        assert_eq!(
            planned(&["zapbrew", "info", "--json=v2"]),
            Plan {
                needs_formula: true,
                needs_cask: true,
                kind: OpKind::Info(zapbrew_ops::info::Args {
                    names: Vec::new(),
                    json_v2: true,
                }),
            }
        );
        assert_eq!(
            planned(&["zapbrew", "info", "wget", "--json=v2"]),
            Plan {
                needs_formula: true,
                needs_cask: true,
                kind: OpKind::Info(zapbrew_ops::info::Args {
                    names: vec!["wget".to_owned()],
                    json_v2: true,
                }),
            }
        );
    }

    #[test]
    fn deps_and_uses_map_edge_flags() {
        assert_eq!(
            planned(&[
                "zapbrew",
                "deps",
                "wget",
                "--tree",
                "--union",
                "--include-build",
                "--include-test",
                "--include-optional",
                "--skip-recommended",
            ])
            .kind,
            OpKind::Deps(zapbrew_ops::deps::Args {
                names: vec!["wget".to_owned()],
                tree: true,
                union: true,
                include_build: true,
                include_test: true,
                include_optional: true,
                skip_recommended: true,
            })
        );
        assert_eq!(
            planned(&["zapbrew", "uses", "wget", "-r", "--installed"]).kind,
            OpKind::Uses(zapbrew_ops::uses::Args {
                names: vec!["wget".to_owned()],
                recursive: true,
                installed: true,
                include_build: false,
                include_test: false,
                include_optional: false,
                skip_recommended: false,
                width: WIDTH,
            })
        );
    }

    #[test]
    fn leaves_filter_mapping() {
        assert_eq!(
            planned(&["zapbrew", "leaves"]).kind,
            OpKind::Leaves(zapbrew_ops::leaves::Args {
                filter: zapbrew_ops::leaves::Filter::All,
            })
        );
        assert_eq!(
            planned(&["zapbrew", "leaves", "-r"]).kind,
            OpKind::Leaves(zapbrew_ops::leaves::Args {
                filter: zapbrew_ops::leaves::Filter::OnRequest,
            })
        );
        assert_eq!(
            planned(&["zapbrew", "leaves", "-p"]).kind,
            OpKind::Leaves(zapbrew_ops::leaves::Args {
                filter: zapbrew_ops::leaves::Filter::AsDependency,
            })
        );
    }

    #[test]
    fn link_cleanup_fetch_field_mapping() {
        assert_eq!(
            planned(&["zapbrew", "link", "wget", "--overwrite", "-n", "-f"]).kind,
            OpKind::Link(zapbrew_ops::link::Args {
                names: vec!["wget".to_owned()],
                overwrite: true,
                dry_run: true,
                force: true,
            })
        );
        assert_eq!(
            planned(&["zapbrew", "cleanup", "wget", "-s", "-n"]).kind,
            OpKind::Cleanup(zapbrew_ops::cleanup::Args {
                names: vec!["wget".to_owned()],
                dry_run: true,
                scrub: true,
                ..Default::default()
            })
        );
        assert_eq!(
            planned(&["zapbrew", "fetch", "wget", "--deps"]).kind,
            OpKind::Fetch(zapbrew_ops::fetch::Args {
                names: vec!["wget".to_owned()],
                deps: true,
                mode: zapbrew_ops::fetch::Mode::Auto,
            })
        );
    }

    #[test]
    fn services_and_shim_action_mapping() {
        assert_eq!(
            planned(&["zapbrew", "services", "start", "mysql", "redis"]).kind,
            OpKind::Services(zapbrew_ops::services::Args {
                action: zapbrew_ops::services::ServiceAction::Start,
                names: vec!["mysql".to_owned(), "redis".to_owned()],
            })
        );
        assert_eq!(
            planned(&["zapbrew", "services", "list"]).kind,
            OpKind::Services(zapbrew_ops::services::Args {
                action: zapbrew_ops::services::ServiceAction::List,
                names: Vec::new(),
            })
        );
        assert_eq!(
            planned(&["zapbrew", "services", "run", "mysql"]).kind,
            OpKind::Services(zapbrew_ops::services::Args {
                action: zapbrew_ops::services::ServiceAction::Run,
                names: vec!["mysql".to_owned()],
            })
        );
        assert_eq!(
            planned(&["zapbrew", "services", "info", "mysql"]).kind,
            OpKind::Services(zapbrew_ops::services::Args {
                action: zapbrew_ops::services::ServiceAction::Info,
                names: vec!["mysql".to_owned()],
            })
        );
        assert_eq!(
            planned(&["zapbrew", "services", "kill", "mysql"]).kind,
            OpKind::Services(zapbrew_ops::services::Args {
                action: zapbrew_ops::services::ServiceAction::Kill,
                names: vec!["mysql".to_owned()],
            })
        );
        assert_eq!(
            planned(&["zapbrew", "services", "cleanup"]).kind,
            OpKind::Services(zapbrew_ops::services::Args {
                action: zapbrew_ops::services::ServiceAction::Cleanup,
                names: Vec::new(),
            })
        );
        assert_eq!(
            planned(&["zapbrew", "services"]).kind,
            OpKind::Services(zapbrew_ops::services::Args {
                action: zapbrew_ops::services::ServiceAction::List,
                names: Vec::new(),
            })
        );
        assert_eq!(
            planned(&["zapbrew", "shim", "install"]).kind,
            OpKind::Shim(zapbrew_ops::shim::Args {
                action: zapbrew_ops::shim::ShimAction::Install,
            })
        );
    }

    #[test]
    fn install_include_test_mapping() {
        assert_eq!(
            planned(&["zapbrew", "install", "wget", "--include-test"]).kind,
            OpKind::Install(zapbrew_ops::install::Args {
                names: vec!["wget".to_owned()],
                only_dependencies: false,
                force: false,
                dry_run: false,
                build_from_source: false,
                head: false,
                interactive: false,
                include_test: true,
            })
        );
    }

    #[test]
    fn postinstall_plans_with_formula_names() {
        assert_eq!(
            planned(&["zapbrew", "postinstall", "wget", "curl"]).kind,
            OpKind::Postinstall(zapbrew_ops::postinstall::Args {
                names: vec!["wget".to_owned(), "curl".to_owned()],
            })
        );
    }

    #[test]
    fn search_selector_drives_classification() {
        assert_eq!(classify(&["zapbrew", "search", "wget"]), (true, true));
        assert_eq!(
            classify(&["zapbrew", "search", "wget", "--formula"]),
            (true, false)
        );
        assert_eq!(
            classify(&["zapbrew", "search", "wget", "--cask"]),
            (false, true)
        );
    }

    #[test]
    fn desc_bare_loads_both_catalogs() {
        // ops desc tries formula then cask, so a bare name that is a cask
        // must find a populated cask catalog — load both by default.
        assert_eq!(classify(&["zapbrew", "desc", "firefox"]), (true, true));
    }

    #[test]
    fn desc_formula_discriminator_loads_only_formula() {
        assert_eq!(
            classify(&["zapbrew", "desc", "wget", "--formula"]),
            (true, false)
        );
    }

    #[test]
    fn desc_cask_discriminator_loads_only_cask() {
        assert_eq!(
            classify(&["zapbrew", "desc", "firefox", "--cask"]),
            (false, true)
        );
    }

    #[test]
    fn desc_search_mode_respects_discriminator() {
        // Search mode without a discriminator scans both catalogs; --cask
        // scopes it to casks only, proving the discriminator (not the search
        // flag) drives classification.
        assert_eq!(
            classify(&["zapbrew", "desc", "--search", "browser"]),
            (true, true)
        );
        assert_eq!(
            classify(&["zapbrew", "desc", "--search", "browser", "--cask"]),
            (false, true)
        );
    }

    #[test]
    fn list_names_drive_formula_classification() {
        assert_eq!(classify(&["zapbrew", "list"]), (false, false));
        assert_eq!(classify(&["zapbrew", "list", "wget"]), (true, false));
    }

    #[test]
    fn local_commands_load_no_catalog() {
        for args in [
            &["zapbrew", "uninstall", "wget"][..],
            &["zapbrew", "leaves"][..],
            &["zapbrew", "autoremove"][..],
            &["zapbrew", "pin", "wget"][..],
            &["zapbrew", "unpin", "wget"][..],
            &["zapbrew", "unlink", "wget"][..],
            &["zapbrew", "config"][..],
            &["zapbrew", "shellenv"][..],
            &["zapbrew", "tap"][..],
            &["zapbrew", "untap", "user/repo"][..],
            &["zapbrew", "tap-info", "user/repo"][..],
            &["zapbrew", "update"][..],
            &["zapbrew", "shim", "remove"][..],
        ] {
            assert_eq!(classify(args), (false, false), "args: {args:?}");
        }
    }

    #[test]
    fn catalog_reading_commands_load_formula() {
        for args in [
            &["zapbrew", "install", "wget"][..],
            &["zapbrew", "install", "wget", "--include-test"][..],
            &["zapbrew", "reinstall", "wget", "--formula"][..],
            &["zapbrew", "deps", "wget"][..],
            &["zapbrew", "link", "wget"][..],
            &["zapbrew", "cleanup"][..],
            &["zapbrew", "doctor"][..],
            &["zapbrew", "postinstall", "wget"][..],
            &["zapbrew", "services", "list"][..],
        ] {
            assert_eq!(classify(args), (true, false), "args: {args:?}");
        }
        for args in [
            &["zapbrew", "outdated"][..],
            &["zapbrew", "info", "wget"][..],
            &["zapbrew", "uses", "firefox"][..],
            &["zapbrew", "fetch", "wget"][..],
            &["zapbrew", "upgrade"][..],
        ] {
            assert_eq!(classify(args), (true, true), "args: {args:?}");
        }
    }

    #[test]
    fn non_utf8_appdir_is_refused() {
        use std::os::unix::ffi::OsStringExt;

        let bad = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![0x66, 0x80, 0x6f]));
        assert!(appdir(Some(bad)).is_err());
        assert!(appdir(None).expect("none").is_none());
    }

    #[test]
    fn suggestion_suffix_formats_zero_one_two() {
        assert_eq!(suggestion_suffix(&[]), "");
        assert_eq!(
            suggestion_suffix(&["wget".to_owned()]),
            " Did you mean wget?"
        );
        assert_eq!(
            suggestion_suffix(&["wget".to_owned(), "wgetpaste".to_owned()]),
            " Did you mean wget or wgetpaste?"
        );
    }

    #[test]
    fn completions_are_refused_by_dispatch() {
        let cli = Cli::parse_from(["zapbrew", "completions", "bash"]);
        let error = plan(cli.command.expect("command"), &cli.globals, WIDTH)
            .expect_err("dispatch must refuse an unintercepted completion");
        assert!(matches!(error, zapbrew_ops::OpError::InvalidState { .. }));
    }

    /// Every flag `cask::install::Args` cannot carry must be refused, not
    /// dropped. The list is the guard's list: a new `InstallArgs` flag that is
    /// plumbed into the formula path but not the cask path fails here.
    #[test]
    fn cask_install_refuses_every_unsupported_flag() {
        for flag in [
            "--only-dependencies",
            "--dry-run",
            "--build-from-source",
            "--HEAD",
            "--interactive",
            "--include-test",
        ] {
            let cli = Cli::parse_from(["zapbrew", "install", "--cask", flag, "firefox"]);
            let error = plan(cli.command.expect("command"), &cli.globals, WIDTH)
                .expect_err("an unsupported flag with --cask must refuse");
            match error {
                zapbrew_ops::OpError::Refusal { message } => assert!(
                    message.contains(flag) && message.contains("--cask"),
                    "refusal must name {flag}: {message}"
                ),
                other => panic!("expected a refusal for {flag}, got {other:?}"),
            }
        }
    }

    #[test]
    fn cask_install_without_unsupported_flags_still_plans() {
        let cli = Cli::parse_from(["zapbrew", "install", "--cask", "--force", "firefox"]);
        let plan = plan(cli.command.expect("command"), &cli.globals, WIDTH)
            .expect("a plain cask install must still plan");
        match plan.kind {
            OpKind::CaskInstall(args) => {
                assert_eq!(args.tokens, vec!["firefox".to_owned()]);
                assert!(args.force, "force must reach cask args");
            }
            other => panic!("expected a cask install, got {other:?}"),
        }
    }

    #[test]
    fn formula_install_dry_run_still_plans() {
        let cli = Cli::parse_from(["zapbrew", "install", "--dry-run", "wget"]);
        let plan = plan(cli.command.expect("command"), &cli.globals, WIDTH)
            .expect("formula dry-run must still plan");
        match plan.kind {
            OpKind::Install(args) => assert!(args.dry_run, "dry_run must reach install args"),
            other => panic!("expected a formula install, got {other:?}"),
        }
    }
    #[test]
    fn install_appdir_is_refused_for_formula_and_mapped_for_cask() {
        let cli = Cli::parse_from(["zapbrew", "install", "wget", "--appdir", "/Apps"]);
        let error = plan(cli.command.expect("command"), &cli.globals, WIDTH)
            .expect_err("formula --appdir must refuse");
        match error {
            zapbrew_ops::OpError::Refusal { message } => assert!(
                message.contains("--appdir") && message.contains("--cask"),
                "refusal must name --appdir and --cask: {message}"
            ),
            other => panic!("expected a refusal for --appdir, got {other:?}"),
        }

        let cask_cli = Cli::parse_from([
            "zapbrew", "install", "--cask", "firefox", "--appdir", "/Apps",
        ]);
        let cask_plan = plan(cask_cli.command.expect("command"), &cask_cli.globals, WIDTH)
            .expect("cask --appdir must still plan");
        match cask_plan.kind {
            OpKind::CaskInstall(args) => {
                assert_eq!(args.tokens, vec!["firefox".to_owned()]);
                assert_eq!(args.appdir, Some("/Apps".into()));
            }
            other => panic!("expected a cask install, got {other:?}"),
        }
    }

    #[test]
    fn fetch_auto_loads_both_catalogs() {
        let plan = planned(&["zapbrew", "fetch", "foo"]);
        assert!(plan.needs_formula);
        assert!(plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Fetch(zapbrew_ops::fetch::Args {
                names: vec!["foo".to_owned()],
                deps: false,
                mode: zapbrew_ops::fetch::Mode::Auto,
            })
        );
    }

    #[test]
    fn fetch_formula_only_loads_formula_catalog() {
        let plan = planned(&["zapbrew", "fetch", "--formula", "foo"]);
        assert!(plan.needs_formula);
        assert!(!plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Fetch(zapbrew_ops::fetch::Args {
                names: vec!["foo".to_owned()],
                deps: false,
                mode: zapbrew_ops::fetch::Mode::FormulaOnly,
            })
        );
    }

    #[test]
    fn fetch_cask_only_loads_cask_catalog() {
        let plan = planned(&["zapbrew", "fetch", "--cask", "foo"]);
        assert!(!plan.needs_formula);
        assert!(plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Fetch(zapbrew_ops::fetch::Args {
                names: vec!["foo".to_owned()],
                deps: false,
                mode: zapbrew_ops::fetch::Mode::CaskOnly,
            })
        );
    }

    #[test]
    fn reinstall_requires_name() {
        let result = Cli::try_parse_from(["zapbrew", "reinstall"]);
        assert!(result.is_err(), "reinstall with no names must fail");
    }

    #[test]
    fn reinstall_formula_and_cask_conflict() {
        let result = Cli::try_parse_from(["zapbrew", "reinstall", "foo", "--formula", "--cask"]);
        assert!(result.is_err(), "--formula and --cask must conflict");
    }

    #[test]
    fn reinstall_formula_appdir_refused() {
        let cli = Cli::parse_from([
            "zapbrew",
            "reinstall",
            "foo",
            "--formula",
            "--appdir",
            "/Apps",
        ]);
        let error = plan(cli.command.expect("command"), &cli.globals, WIDTH)
            .expect_err("--appdir with --formula must refuse");
        match error {
            zapbrew_ops::OpError::Refusal { message } => {
                assert!(
                    message.contains("--appdir"),
                    "refusal must name --appdir: {message}"
                )
            }
            other => panic!("expected a refusal for --appdir, got {other:?}"),
        }
    }

    #[test]
    fn reinstall_auto_loads_both_catalogs() {
        let plan = planned(&["zapbrew", "reinstall", "foo"]);
        assert!(plan.needs_formula);
        assert!(plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Reinstall(zapbrew_ops::reinstall::Args {
                names: vec!["foo".to_owned()],
                formula: false,
                cask: false,
                appdir: None,
            })
        );
    }

    #[test]
    fn reinstall_formula_only_loads_formula_catalog() {
        let plan = planned(&["zapbrew", "reinstall", "foo", "--formula"]);
        assert!(plan.needs_formula);
        assert!(!plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Reinstall(zapbrew_ops::reinstall::Args {
                names: vec!["foo".to_owned()],
                formula: true,
                cask: false,
                appdir: None,
            })
        );
    }

    #[test]
    fn reinstall_cask_only_loads_cask_catalog() {
        let plan = planned(&["zapbrew", "reinstall", "foo", "--cask"]);
        assert!(!plan.needs_formula);
        assert!(plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Reinstall(zapbrew_ops::reinstall::Args {
                names: vec!["foo".to_owned()],
                formula: false,
                cask: true,
                appdir: None,
            })
        );
    }

    #[test]
    fn reinstall_cask_maps_appdir_and_forces() {
        let plan = planned(&[
            "zapbrew",
            "reinstall",
            "--cask",
            "firefox",
            "--appdir",
            "/Apps",
        ]);
        assert!(!plan.needs_formula);
        assert!(plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Reinstall(zapbrew_ops::reinstall::Args {
                names: vec!["firefox".to_owned()],
                formula: false,
                cask: true,
                appdir: Some("/Apps".into()),
            })
        );
    }
    #[test]
    fn upgrade_auto_maps_every_field_and_loads_both_catalogs() {
        let plan = planned(&[
            "zapbrew",
            "upgrade",
            "foo",
            "--dry-run",
            "--greedy",
            "--greedy-latest",
            "--greedy-auto-updates",
        ]);
        assert!(plan.needs_formula);
        assert!(plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Upgrade(zapbrew_ops::upgrade::Args {
                names: vec!["foo".to_owned()],
                dry_run: true,
                mode: zapbrew_ops::upgrade::Mode::Auto,
                appdir: None,
                greedy: true,
                greedy_latest: true,
                greedy_auto_updates: true,
            })
        );
    }

    #[test]
    fn upgrade_cask_only_maps_appdir_and_loads_casks() {
        let plan = planned(&[
            "zapbrew", "upgrade", "--cask", "firefox", "--appdir", "/Apps",
        ]);
        assert!(!plan.needs_formula);
        assert!(plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Upgrade(zapbrew_ops::upgrade::Args {
                names: vec!["firefox".to_owned()],
                mode: zapbrew_ops::upgrade::Mode::Cask,
                appdir: Some("/Apps".into()),
                ..zapbrew_ops::upgrade::Args::default()
            })
        );
    }

    #[test]
    fn upgrade_formula_only_loads_formula_catalog() {
        let plan = planned(&["zapbrew", "upgrade", "--formula", "foo"]);
        assert!(plan.needs_formula);
        assert!(!plan.needs_cask);
        assert_eq!(
            plan.kind,
            OpKind::Upgrade(zapbrew_ops::upgrade::Args {
                names: vec!["foo".to_owned()],
                mode: zapbrew_ops::upgrade::Mode::Formula,
                ..zapbrew_ops::upgrade::Args::default()
            })
        );
    }
}
