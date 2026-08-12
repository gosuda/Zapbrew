//! Complete `zapbrew` command-line surface.
//!
//! Task 7a: the full typed clap 4.6 parse surface. Every approved verb, flag,
//! positional, value, conflict, required group, and alias is represented as
//! plain, public clap data. Conversion into the `zapbrew-ops` `Args` types,
//! output, dispatch, catalog loading, and fast-path resolution belong to later
//! Task 7 slices and are intentionally absent here.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Rewrite common Homebrew command aliases and legacy completion shortcuts to
/// their canonical names.
///
/// This operates on the argument vector (including the binary name at index 0)
/// and returns a newly-allocated vector. It runs before clap so that `ls`,
/// `rm`, `-S`, `up`, etc. behave the same in the binary and in unit tests.
pub(crate) fn canonicalize_argv(argv: Vec<String>) -> Vec<String> {
    let mut argv = argv;
    if let Some(first) = argv.get_mut(1) {
        *first = match first.as_str() {
            "ls" => "list".to_owned(),
            "-S" => "search".to_owned(),
            "up" => "update".to_owned(),
            "ln" => "link".to_owned(),
            "instal" => "install".to_owned(),
            "uninstal" | "rm" | "remove" => "uninstall".to_owned(),
            "abv" => "info".to_owned(),
            "dr" => "doctor".to_owned(),
            other => other.to_owned(),
        };
    }

    argv
}

/// Root parser for the `zapbrew` binary.
#[derive(Debug, Parser)]
#[command(
    name = "zapbrew",
    version = concat!(env!("CARGO_PKG_VERSION"), " (Homebrew 5-compatible)"),
    about = "Homebrew 5-compatible package manager",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub globals: GlobalArgs,

    /// Print the install prefix, optionally for a formula. Resolved later.
    #[arg(long, num_args = 0..=1, value_name = "formula")]
    pub prefix: Option<Option<String>>,

    /// Print the Cellar path, optionally for a formula. Resolved later.
    #[arg(long, num_args = 0..=1, value_name = "formula")]
    pub cellar: Option<Option<String>>,

    /// Print the Caskroom path, optionally for a cask token. Resolved later.
    #[arg(long, num_args = 0..=1, value_name = "cask")]
    pub caskroom: Option<Option<String>>,

    /// Print the download cache path, optionally for a formula. Resolved later.
    #[arg(long, num_args = 0..=1, value_name = "formula")]
    pub cache: Option<Option<String>>,

    /// Print the repository path, optionally for a tap. Resolved later.
    #[arg(long, num_args = 0..=1, value_name = "tap")]
    pub repository: Option<Option<String>>,

    /// Print the Taps directory path.
    #[arg(long)]
    pub taps: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Globals accepted before or after any subcommand.
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Enable debug output.
    #[arg(long, global = true)]
    pub debug: bool,

    /// Suppress non-essential output.
    #[arg(short = 'q', long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Enable verbose output.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,
}

/// JSON output version. `zapbrew` emits the v2 schema only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum JsonVersion {
    V2,
}

/// Shell dialect the explicitly named completion generator supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

/// Every approved top-level verb.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Install formulae or casks.
    Install(InstallArgs),
    /// Reinstall formulae.
    Reinstall(NamesArgs),
    /// Uninstall formulae or casks.
    Uninstall(UninstallArgs),
    /// Upgrade outdated formulae.
    Upgrade(UpgradeArgs),
    /// List outdated formulae and casks.
    Outdated(OutdatedArgs),
    /// List installed formulae or casks.
    List(ListArgs),
    /// Show information about formulae and casks.
    Info(InfoArgs),
    /// Show dependencies of formulae.
    Deps(DepsArgs),
    /// Show formulae and casks that depend on the named formulae or casks.
    Uses(UsesArgs),
    /// List installed formulae not required by others.
    Leaves(LeavesArgs),
    /// Uninstall formulae that are no longer needed.
    Autoremove(AutoremoveArgs),
    /// Pin formulae, preventing upgrades.
    Pin(NamesArgs),
    /// Unpin formulae, allowing upgrades.
    Unpin(NamesArgs),
    /// Symlink a keg's files into the prefix.
    Link(LinkArgs),
    /// Remove a keg's symlinks from the prefix.
    Unlink(UnlinkArgs),
    /// Download bottles without installing.
    Fetch(FetchArgs),
    /// Remove stale downloads and old versions.
    Cleanup(CleanupArgs),
    /// Search for formulae and casks.
    Search(SearchArgs),
    /// Show descriptions or search names/descriptions of formulae/casks.
    Desc(DescArgs),
    /// Rerun post-install steps for installed formulae.
    Postinstall(NamesArgs),
    /// Show the effective configuration.
    Config,
    /// Print shell integration for the environment.
    Shellenv(ShellenvArgs),
    /// Tap a formula repository, or list taps.
    Tap(TapArgs),
    /// Remove a tapped repository.
    Untap(UntapArgs),
    /// Show information about a tap.
    TapInfo(TapInfoArgs),
    /// Check the system for potential problems.
    Doctor,
    /// Manage background services.
    Services(ServicesArgs),
    /// Fetch the latest catalog and taps.
    Update,
    /// Manage the opt-in `brew` shim.
    Shim(ShimArgs),
    /// Manage shell completion links.
    Completions(CompletionsArgs),
}

/// Shared arguments for verbs that take only a list of names.
#[derive(Debug, Args)]
pub struct NamesArgs {
    /// Formula names.
    pub names: Vec<String>,
}

/// Homebrew `desc` arguments: named lookup or regex search across names/descriptions.
#[derive(Debug, Args)]
pub struct DescArgs {
    /// Formula, cask, or search text.
    pub names: Vec<String>,

    /// Search names and descriptions for <text>.
    #[arg(short = 's', long, group = "search_mode")]
    pub search: Option<String>,

    /// Search only names.
    #[arg(short = 'n', long, group = "search_mode")]
    pub search_name: Option<String>,

    /// Search only descriptions.
    #[arg(short = 'd', long, group = "search_mode")]
    pub search_description: Option<String>,

    /// Treat named arguments as formulae.
    #[arg(long)]
    pub formula: bool,

    /// Treat named arguments as casks.
    #[arg(long, conflicts_with = "formula")]
    pub cask: bool,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Formula or cask names to install.
    pub names: Vec<String>,
    /// Install only the dependencies, not the formulae themselves.
    #[arg(long)]
    pub only_dependencies: bool,
    /// Install even if already installed.
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Show what would be installed without doing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Compile from source instead of pouring a bottle.
    #[arg(long)]
    pub build_from_source: bool,
    /// Install the HEAD version.
    #[arg(long = "HEAD")]
    pub head: bool,
    /// Run the installation interactively.
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Include test dependencies during expansion.
    #[arg(long)]
    pub include_test: bool,
    /// Treat the named arguments as casks (macOS only).
    #[arg(long, conflicts_with = "formula")]
    pub cask: bool,
    /// Treat the named arguments as formulae.
    #[arg(long)]
    pub formula: bool,
    /// Target application directory for cask apps.
    #[arg(long, value_name = "DIR")]
    pub appdir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Formula or cask names to uninstall.
    pub names: Vec<String>,
    /// Delete all installed versions, not just the active one.
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Do not check for dependents before uninstalling.
    #[arg(long)]
    pub ignore_dependencies: bool,
    /// Treat the named arguments as casks (macOS only).
    #[arg(long, conflicts_with = "formula")]
    pub cask: bool,
    /// Treat the named arguments as formulae.
    #[arg(long)]
    pub formula: bool,
    /// Also remove all files a cask created (cask only).
    #[arg(long, requires = "cask")]
    pub zap: bool,
}

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// Formula names to upgrade; empty upgrades all.
    pub names: Vec<String>,
    /// Show what would be upgraded without doing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct OutdatedArgs {
    /// Formula or cask names to check; empty checks all installed formulae and casks.
    pub names: Vec<String>,
    /// Emit JSON output (v2 schema).
    #[arg(long, value_name = "v2", num_args = 0..=1, require_equals = true, default_missing_value = "v2")]
    pub json: Option<JsonVersion>,
    /// Include casks with auto-updates or `latest` versions.
    #[arg(long)]
    pub greedy: bool,
    /// Include casks whose version is `latest`.
    #[arg(long)]
    pub greedy_latest: bool,
    /// Include casks that update themselves.
    #[arg(long)]
    pub greedy_auto_updates: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Formula or cask names to list; empty lists all formulae unless --cask is set.
    pub names: Vec<String>,
    /// Show version numbers.
    #[arg(long)]
    pub versions: bool,
    /// Print one entry per line.
    #[arg(short = '1')]
    pub oneline: bool,
    /// List casks instead of formulae (macOS only).
    #[arg(long, conflicts_with = "formula")]
    pub cask: bool,
    /// Treat the named arguments as formulae.
    #[arg(long)]
    pub formula: bool,
}

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Formula or cask names to describe.
    pub names: Vec<String>,
    /// Emit JSON output (v2 schema).
    #[arg(long, value_name = "v2", num_args = 0..=1, require_equals = true, default_missing_value = "v2")]
    pub json: Option<JsonVersion>,
}

#[derive(Debug, Args)]
pub struct DepsArgs {
    /// Formula names.
    pub names: Vec<String>,
    /// Render the dependency graph as a tree.
    #[arg(long)]
    pub tree: bool,
    /// Show the union of dependencies across all arguments.
    #[arg(long)]
    pub union: bool,
    /// Include `:build` dependencies.
    #[arg(long)]
    pub include_build: bool,
    /// Include `:test` dependencies.
    #[arg(long)]
    pub include_test: bool,
    /// Include `:optional` dependencies.
    #[arg(long)]
    pub include_optional: bool,
    /// Skip `:recommended` dependencies.
    #[arg(long)]
    pub skip_recommended: bool,
}

#[derive(Debug, Args)]
pub struct UsesArgs {
    /// Formula or cask names.
    pub names: Vec<String>,
    /// Resolve the reverse-dependency closure recursively.
    #[arg(short = 'r', long)]
    pub recursive: bool,
    /// Restrict results to installed formulae and casks.
    #[arg(long)]
    pub installed: bool,
    /// Include `:build` dependencies.
    #[arg(long)]
    pub include_build: bool,
    /// Include `:test` dependencies.
    #[arg(long)]
    pub include_test: bool,
    /// Include `:optional` dependencies.
    #[arg(long)]
    pub include_optional: bool,
    /// Skip `:recommended` dependencies.
    #[arg(long)]
    pub skip_recommended: bool,
}

#[derive(Debug, Args)]
pub struct LeavesArgs {
    /// Only list formulae installed on request.
    #[arg(short = 'r', long, conflicts_with = "installed_as_dependency")]
    pub installed_on_request: bool,
    /// Only list formulae installed as a dependency.
    #[arg(short = 'p', long)]
    pub installed_as_dependency: bool,
}

#[derive(Debug, Args)]
pub struct AutoremoveArgs {
    /// Show what would be removed without doing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct LinkArgs {
    /// Formula names to link.
    pub names: Vec<String>,
    /// Delete files that already exist in the prefix.
    #[arg(long)]
    pub overwrite: bool,
    /// Show what would be linked without doing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Force linking even for keg-only formulae.
    #[arg(short = 'f', long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct UnlinkArgs {
    /// Formula names to unlink.
    pub names: Vec<String>,
    /// Show what would be removed without doing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Formula or cask names to fetch.
    #[arg(required = true)]
    pub names: Vec<String>,
    /// Fetch formulae only.
    #[arg(long, visible_alias = "formulae")]
    pub formula: bool,
    /// Fetch casks only.
    #[arg(long, visible_alias = "casks", conflicts_with = "formula")]
    pub cask: bool,
    /// Also fetch the dependency closure.
    #[arg(long, conflicts_with = "cask")]
    pub deps: bool,
}

#[derive(Debug, Args)]
pub struct CleanupArgs {
    /// Formula names to clean; empty cleans all.
    pub names: Vec<String>,
    /// Scrub the download cache of the latest versions too.
    #[arg(short = 's', long)]
    pub scrub: bool,
    /// Show what would be removed without doing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Remove all cache files older than <days>, or 'all'.
    #[arg(long, value_name = "days")]
    pub prune: Option<String>,
    /// Only prune the symlinks and directories from the prefix.
    #[arg(long)]
    pub prune_prefix: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Search term, or `/regex/`.
    pub query: String,
    /// Search descriptions as well as names.
    #[arg(long)]
    pub desc: bool,
    /// Restrict the search to formulae.
    #[arg(long, conflicts_with = "cask")]
    pub formula: bool,
    /// Restrict the search to casks.
    #[arg(long)]
    pub cask: bool,
}

#[derive(Debug, Args)]
pub struct ShellenvArgs {
    /// Shell template: bash/sh, zsh, fish, csh/tcsh, or pwsh; other names use POSIX. Defaults to the current shell.
    pub shell: Option<String>,
}

#[derive(Debug, Args)]
pub struct TapArgs {
    /// Tap name, e.g. `user/repo`. Omit to list taps.
    pub name: Option<String>,
    /// Remote URL to clone the tap from.
    pub url: Option<String>,
    /// Force the tap even for a built-in name.
    #[arg(short = 'f', long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct UntapArgs {
    /// Tap names to remove.
    pub names: Vec<String>,
    /// Remove even if formulae from the tap are installed.
    #[arg(short = 'f', long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct TapInfoArgs {
    /// Tap names to describe.
    pub names: Vec<String>,
    /// Show information for every installed tap.
    #[arg(long)]
    pub installed: bool,
    /// Emit JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ServicesArgs {
    #[command(subcommand)]
    pub command: Option<ServicesCommand>,
}

/// Service management actions.
#[derive(Debug, Subcommand)]
pub enum ServicesCommand {
    /// List services and their status.
    List,
    /// Start services.
    Start {
        /// Service names to start.
        names: Vec<String>,
    },
    /// Stop services.
    Stop {
        /// Service names to stop.
        names: Vec<String>,
    },
    /// Restart services.
    Restart {
        /// Service names to restart.
        names: Vec<String>,
    },
    /// Run a service without registering it to launch at login or boot.
    Run {
        /// Service names to run.
        names: Vec<String>,
    },
    /// Print one-line status for services.
    Info {
        /// Service names to inspect.
        names: Vec<String>,
    },
    /// Stop a service but keep it registered.
    Kill {
        /// Service names to kill.
        names: Vec<String>,
    },
    /// Remove unused service files.
    Cleanup,
}

#[derive(Debug, Args)]
pub struct ShimArgs {
    #[command(subcommand)]
    pub command: ShimCommand,
}

/// `brew` shim actions.
#[derive(Debug, Subcommand)]
pub enum ShimCommand {
    /// Install the `<prefix>/bin/brew` shim.
    Install,
    /// Remove the `<prefix>/bin/brew` shim.
    Remove,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Completion link operation; omit to display the current state.
    #[command(subcommand)]
    pub command: Option<CompletionsCommand>,
    /// Legacy shell shortcut, treated as `completions generate <shell>` when no
    /// subcommand is given.
    #[arg(value_enum)]
    pub shell: Option<CompletionShell>,
}

/// Shell completion link operations.
#[derive(Debug, Subcommand)]
pub enum CompletionsCommand {
    /// Display the current completion link state.
    State,
    /// Link discovered completion files into the active prefix.
    Link,
    /// Remove only links to discovered completion files.
    Unlink,
    /// Generate a completion script from the live Clap surface.
    Generate {
        /// Shell to generate a completion script for.
        #[arg(value_enum)]
        shell: CompletionShell,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    fn parse(argv: &[&str]) -> Cli {
        let argv = canonicalize_argv(argv.iter().map(|&s| s.to_owned()).collect());
        Cli::try_parse_from(argv).expect("expected parse to succeed")
    }

    fn parse_kind(argv: &[&str]) -> ErrorKind {
        let argv = canonicalize_argv(argv.iter().map(|&s| s.to_owned()).collect());
        Cli::try_parse_from(argv)
            .expect_err("expected parse to fail")
            .kind()
    }

    fn command(argv: &[&str]) -> Commands {
        parse(argv).command.expect("expected a subcommand")
    }

    fn command_name(cmd: &Commands) -> &'static str {
        match cmd {
            Commands::Install(_) => "install",
            Commands::Reinstall(_) => "reinstall",
            Commands::Uninstall(_) => "uninstall",
            Commands::Upgrade(_) => "upgrade",
            Commands::Outdated(_) => "outdated",
            Commands::List(_) => "list",
            Commands::Info(_) => "info",
            Commands::Deps(_) => "deps",
            Commands::Uses(_) => "uses",
            Commands::Leaves(_) => "leaves",
            Commands::Autoremove(_) => "autoremove",
            Commands::Pin(_) => "pin",
            Commands::Unpin(_) => "unpin",
            Commands::Link(_) => "link",
            Commands::Unlink(_) => "unlink",
            Commands::Fetch(_) => "fetch",
            Commands::Cleanup(_) => "cleanup",
            Commands::Search(_) => "search",
            Commands::Desc(_) => "desc",
            Commands::Postinstall(_) => "postinstall",
            Commands::Config => "config",
            Commands::Shellenv(_) => "shellenv",
            Commands::Tap(_) => "tap",
            Commands::Untap(_) => "untap",
            Commands::TapInfo(_) => "tap-info",
            Commands::Doctor => "doctor",
            Commands::Services(_) => "services",
            Commands::Update => "update",
            Commands::Shim(_) => "shim",
            Commands::Completions(_) => "completions",
        }
    }

    /// One representative parse per approved verb. Adding a verb without a row
    /// here (or dropping one) makes the surface gap obvious.
    const COMMAND_MATRIX: &[(&[&str], &str)] = &[
        (&["zapbrew", "install", "wget"], "install"),
        (&["zapbrew", "reinstall", "wget"], "reinstall"),
        (&["zapbrew", "uninstall", "wget"], "uninstall"),
        (&["zapbrew", "upgrade"], "upgrade"),
        (&["zapbrew", "outdated"], "outdated"),
        (&["zapbrew", "list"], "list"),
        (&["zapbrew", "info", "wget"], "info"),
        (&["zapbrew", "deps", "wget"], "deps"),
        (&["zapbrew", "uses", "wget"], "uses"),
        (&["zapbrew", "leaves"], "leaves"),
        (&["zapbrew", "autoremove"], "autoremove"),
        (&["zapbrew", "pin", "wget"], "pin"),
        (&["zapbrew", "unpin", "wget"], "unpin"),
        (&["zapbrew", "link", "wget"], "link"),
        (&["zapbrew", "unlink", "wget"], "unlink"),
        (&["zapbrew", "fetch", "wget"], "fetch"),
        (&["zapbrew", "cleanup"], "cleanup"),
        (&["zapbrew", "search", "wget"], "search"),
        (&["zapbrew", "desc", "wget"], "desc"),
        (&["zapbrew", "postinstall", "wget"], "postinstall"),
        (&["zapbrew", "config"], "config"),
        (&["zapbrew", "shellenv"], "shellenv"),
        (&["zapbrew", "tap"], "tap"),
        (&["zapbrew", "untap", "user/repo"], "untap"),
        (&["zapbrew", "tap-info", "user/repo"], "tap-info"),
        (&["zapbrew", "doctor"], "doctor"),
        (&["zapbrew", "services", "list"], "services"),
        (&["zapbrew", "update"], "update"),
        (&["zapbrew", "shim", "install"], "shim"),
        (&["zapbrew", "completions", "bash"], "completions"),
    ];

    #[test]
    fn command_matrix_covers_every_verb() {
        for (argv, expected) in COMMAND_MATRIX {
            let cmd = command(argv);
            assert_eq!(command_name(&cmd), *expected, "argv: {argv:?}");
        }
    }

    #[test]
    fn debug_assert_valid_definition() {
        // clap panics here if any attribute combination is malformed.
        Cli::command().debug_assert();
    }

    #[test]
    fn no_args_prints_help() {
        assert_eq!(
            parse_kind(&["zapbrew"]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn unknown_command_errors() {
        assert_eq!(
            parse_kind(&["zapbrew", "frobnicate"]),
            ErrorKind::InvalidSubcommand
        );
    }

    #[test]
    fn version_line_is_homebrew_compatible() {
        let rendered = Cli::try_parse_from(["zapbrew", "--version"])
            .expect_err("version exits via error channel")
            .to_string();
        assert_eq!(
            rendered.trim_end(),
            format!(
                "zapbrew {} (Homebrew 5-compatible)",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn short_version_flag_matches_long() {
        assert_eq!(parse_kind(&["zapbrew", "-V"]), ErrorKind::DisplayVersion);
    }

    #[test]
    fn help_flag_and_subcommand() {
        assert_eq!(parse_kind(&["zapbrew", "--help"]), ErrorKind::DisplayHelp);
        assert_eq!(parse_kind(&["zapbrew", "-h"]), ErrorKind::DisplayHelp);
        assert_eq!(
            parse_kind(&["zapbrew", "help", "install"]),
            ErrorKind::DisplayHelp
        );
    }

    #[test]
    fn every_subcommand_renders_help() {
        for (_, name) in COMMAND_MATRIX {
            assert_eq!(
                parse_kind(&["zapbrew", name, "--help"]),
                ErrorKind::DisplayHelp,
                "help for {name}"
            );
        }
    }

    #[test]
    fn command_aliases_resolve_to_canonical_verbs() {
        assert_eq!(command_name(&command(&["zapbrew", "ls"])), "list");
        assert_eq!(
            command_name(&command(&["zapbrew", "rm", "wget"])),
            "uninstall"
        );
        assert_eq!(command_name(&command(&["zapbrew", "up"])), "update");
        assert_eq!(command_name(&command(&["zapbrew", "-S", "wget"])), "search");
    }

    #[test]
    fn globals_before_and_after_subcommand() {
        let before = parse(&["zapbrew", "--debug", "-q", "install", "wget"]);
        assert!(before.globals.debug);
        assert!(before.globals.quiet);
        assert!(!before.globals.verbose);

        let after = parse(&["zapbrew", "install", "wget", "--verbose"]);
        assert!(after.globals.verbose);
    }

    #[test]
    fn quiet_conflicts_with_verbose() {
        assert_eq!(
            parse_kind(&["zapbrew", "-q", "-v"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn top_level_fast_paths_parse() {
        let bare = parse(&["zapbrew", "--prefix"]);
        assert_eq!(bare.prefix, Some(None));
        assert!(bare.command.is_none());

        let valued = parse(&["zapbrew", "--prefix", "wget"]);
        assert_eq!(valued.prefix, Some(Some("wget".to_owned())));

        assert_eq!(parse(&["zapbrew", "--cellar"]).cellar, Some(None));
        assert_eq!(
            parse(&["zapbrew", "--cache", "wget"]).cache,
            Some(Some("wget".to_owned()))
        );
        assert_eq!(
            parse(&["zapbrew", "--repository", "user/repo"]).repository,
            Some(Some("user/repo".to_owned()))
        );
    }

    #[test]
    fn install_flags_and_positionals() {
        let Commands::Install(args) = command(&[
            "zapbrew",
            "install",
            "-f",
            "-n",
            "-i",
            "--only-dependencies",
            "--build-from-source",
            "--HEAD",
            "wget",
            "curl",
        ]) else {
            panic!("expected install");
        };
        assert_eq!(args.names, vec!["wget", "curl"]);
        assert!(args.force);
        assert!(args.dry_run);
        assert!(args.interactive);
        assert!(args.only_dependencies);
        assert!(args.build_from_source);
        assert!(args.head);
        assert!(!args.cask);
    }

    #[test]
    fn install_cask_selection() {
        let Commands::Install(args) =
            command(&["zapbrew", "install", "--cask", "--appdir", "/A", "firefox"])
        else {
            panic!("expected install");
        };
        assert!(args.cask);
        assert_eq!(args.appdir.as_deref(), Some(std::path::Path::new("/A")));
        assert_eq!(args.names, vec!["firefox"]);
    }

    #[test]
    fn cask_and_formula_conflict() {
        assert_eq!(
            parse_kind(&["zapbrew", "install", "--cask", "--formula", "x"]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            parse_kind(&["zapbrew", "uninstall", "--cask", "--formula", "x"]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            parse_kind(&["zapbrew", "list", "--cask", "--formula"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn uninstall_zap_requires_cask() {
        let Commands::Uninstall(args) =
            command(&["zapbrew", "uninstall", "--cask", "--zap", "firefox"])
        else {
            panic!("expected uninstall");
        };
        assert!(args.cask);
        assert!(args.zap);

        assert_eq!(
            parse_kind(&["zapbrew", "uninstall", "--zap", "firefox"]),
            ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn outdated_json_value_seam() {
        let Commands::Outdated(bare) = command(&["zapbrew", "outdated", "--json"]) else {
            panic!("expected outdated");
        };
        assert_eq!(bare.json, Some(JsonVersion::V2));

        let Commands::Outdated(valued) = command(&["zapbrew", "outdated", "--json=v2"]) else {
            panic!("expected outdated");
        };
        assert_eq!(valued.json, Some(JsonVersion::V2));

        let Commands::Outdated(none) = command(&["zapbrew", "outdated"]) else {
            panic!("expected outdated");
        };
        assert_eq!(none.json, None);

        assert_eq!(
            parse_kind(&["zapbrew", "outdated", "--json=v1"]),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn info_json_does_not_swallow_positional() {
        // `require_equals` keeps `--json` from consuming the following name.
        let Commands::Info(args) = command(&["zapbrew", "info", "--json", "wget"]) else {
            panic!("expected info");
        };
        assert_eq!(args.json, Some(JsonVersion::V2));
        assert_eq!(args.names, vec!["wget"]);
    }

    #[test]
    fn list_oneline_short_flag() {
        let Commands::List(args) = command(&["zapbrew", "list", "-1", "--versions"]) else {
            panic!("expected list");
        };
        assert!(args.oneline);
        assert!(args.versions);
    }

    #[test]
    fn leaves_filter_flags_conflict() {
        let Commands::Leaves(on_request) = command(&["zapbrew", "leaves", "-r"]) else {
            panic!("expected leaves");
        };
        assert!(on_request.installed_on_request);
        assert!(!on_request.installed_as_dependency);

        let Commands::Leaves(as_dep) = command(&["zapbrew", "leaves", "-p"]) else {
            panic!("expected leaves");
        };
        assert!(as_dep.installed_as_dependency);

        assert_eq!(
            parse_kind(&["zapbrew", "leaves", "-r", "-p"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn deps_filter_flags() {
        let Commands::Deps(args) = command(&[
            "zapbrew",
            "deps",
            "--tree",
            "--union",
            "--include-build",
            "--include-test",
            "--include-optional",
            "--skip-recommended",
            "wget",
        ]) else {
            panic!("expected deps");
        };
        assert!(args.tree);
        assert!(args.union);
        assert!(args.include_build);
        assert!(args.include_test);
        assert!(args.include_optional);
        assert!(args.skip_recommended);
    }

    #[test]
    fn search_query_required_and_conflicts() {
        let Commands::Search(args) = command(&["zapbrew", "search", "--desc", "foo"]) else {
            panic!("expected search");
        };
        assert_eq!(args.query, "foo");
        assert!(args.desc);

        assert_eq!(
            parse_kind(&["zapbrew", "search"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_kind(&["zapbrew", "search", "--formula", "--cask", "foo"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn tap_optional_positionals() {
        let Commands::Tap(bare) = command(&["zapbrew", "tap"]) else {
            panic!("expected tap");
        };
        assert_eq!(bare.name, None);
        assert_eq!(bare.url, None);

        let Commands::Tap(named) = command(&["zapbrew", "tap", "-f", "user/repo", "https://x"])
        else {
            panic!("expected tap");
        };
        assert_eq!(named.name.as_deref(), Some("user/repo"));
        assert_eq!(named.url.as_deref(), Some("https://x"));
        assert!(named.force);
    }

    #[test]
    fn tap_info_json_is_plain_bool() {
        let Commands::TapInfo(args) =
            command(&["zapbrew", "tap-info", "--installed", "--json", "user/repo"])
        else {
            panic!("expected tap-info");
        };
        assert!(args.installed);
        assert!(args.json);
        assert_eq!(args.names, vec!["user/repo"]);
    }

    #[test]
    fn services_nested_subcommands() {
        let Commands::Services(list) = command(&["zapbrew", "services", "list"]) else {
            panic!("expected services");
        };
        assert!(matches!(list.command, Some(ServicesCommand::List)));

        let Commands::Services(start) = command(&["zapbrew", "services", "start", "a", "b"]) else {
            panic!("expected services");
        };
        let Some(ServicesCommand::Start { names }) = start.command else {
            panic!("expected start");
        };
        assert_eq!(names, vec!["a", "b"]);

        let Commands::Services(run) = command(&["zapbrew", "services", "run", "a"]) else {
            panic!("expected services");
        };
        let Some(ServicesCommand::Run { names }) = run.command else {
            panic!("expected run");
        };
        assert_eq!(names, vec!["a"]);

        let Commands::Services(info) = command(&["zapbrew", "services", "info", "a"]) else {
            panic!("expected services");
        };
        let Some(ServicesCommand::Info { names }) = info.command else {
            panic!("expected info");
        };
        assert_eq!(names, vec!["a"]);

        let Commands::Services(kill) = command(&["zapbrew", "services", "kill", "a"]) else {
            panic!("expected services");
        };
        let Some(ServicesCommand::Kill { names }) = kill.command else {
            panic!("expected kill");
        };
        assert_eq!(names, vec!["a"]);

        let Commands::Services(cleanup) = command(&["zapbrew", "services", "cleanup"]) else {
            panic!("expected services");
        };
        assert!(matches!(cleanup.command, Some(ServicesCommand::Cleanup)));

        let Commands::Services(default) = command(&["zapbrew", "services"]) else {
            panic!("expected services");
        };
        assert!(default.command.is_none());
    }

    #[test]
    fn shim_nested_subcommands() {
        let Commands::Shim(install) = command(&["zapbrew", "shim", "install"]) else {
            panic!("expected shim");
        };
        assert!(matches!(install.command, ShimCommand::Install));

        let Commands::Shim(remove) = command(&["zapbrew", "shim", "remove"]) else {
            panic!("expected shim");
        };
        assert!(matches!(remove.command, ShimCommand::Remove));

        assert_eq!(
            parse_kind(&["zapbrew", "shim"]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn shellenv_optional_shell() {
        let Commands::Shellenv(bare) = command(&["zapbrew", "shellenv"]) else {
            panic!("expected shellenv");
        };
        assert_eq!(bare.shell, None);

        let Commands::Shellenv(named) = command(&["zapbrew", "shellenv", "fish"]) else {
            panic!("expected shellenv");
        };
        assert_eq!(named.shell.as_deref(), Some("fish"));
    }

    #[test]
    fn completions_parses_state_link_unlink_and_both_generation_forms() {
        for argv in [
            &["zapbrew", "completions"][..],
            &["zapbrew", "completions", "state"][..],
            &["zapbrew", "completions", "link"][..],
            &["zapbrew", "completions", "unlink"][..],
        ] {
            let Commands::Completions(args) = command(argv) else {
                panic!("expected completions for {argv:?}");
            };
            assert!(args.shell.is_none(), "argv: {argv:?}");
        }

        let Commands::Completions(bare) = command(&["zapbrew", "completions"]) else {
            panic!("expected bare completions");
        };
        assert!(bare.command.is_none());
        assert!(bare.shell.is_none());

        for (argv, expected) in [
            (
                &["zapbrew", "completions", "generate", "bash"][..],
                CompletionShell::Bash,
            ),
            (
                &["zapbrew", "completions", "generate", "zsh"][..],
                CompletionShell::Zsh,
            ),
            (
                &["zapbrew", "completions", "generate", "fish"][..],
                CompletionShell::Fish,
            ),
        ] {
            let Commands::Completions(args) = command(argv) else {
                panic!("expected completions for {argv:?}");
            };
            assert!(args.shell.is_none(), "argv: {argv:?}");
            let Some(CompletionsCommand::Generate { shell }) = args.command else {
                panic!("expected generate for {argv:?}");
            };
            assert_eq!(shell, expected, "argv: {argv:?}");
        }

        for (argv, expected) in [
            (
                &["zapbrew", "completions", "bash"][..],
                CompletionShell::Bash,
            ),
            (&["zapbrew", "completions", "zsh"][..], CompletionShell::Zsh),
            (
                &["zapbrew", "completions", "fish"][..],
                CompletionShell::Fish,
            ),
        ] {
            let Commands::Completions(args) = command(argv) else {
                panic!("expected completions for {argv:?}");
            };
            assert!(args.command.is_none(), "argv: {argv:?}");
            assert_eq!(args.shell, Some(expected), "argv: {argv:?}");
        }

        for (argv, expected) in [
            (
                &["zapbrew", "completions", "powershell"][..],
                ErrorKind::InvalidValue,
            ),
            (
                &["zapbrew", "completions", "generate", "powershell"][..],
                ErrorKind::InvalidValue,
            ),
        ] {
            assert_eq!(parse_kind(argv), expected, "argv: {argv:?}");
        }
    }

    #[test]
    fn fetch_requires_at_least_one_name() {
        assert_eq!(
            parse_kind(&["zapbrew", "fetch"]),
            ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn fetch_formula_and_cask_flags_are_mutually_exclusive() {
        assert_eq!(
            parse_kind(&["zapbrew", "fetch", "--formula", "--cask", "foo"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn fetch_cask_conflicts_with_deps() {
        assert_eq!(
            parse_kind(&["zapbrew", "fetch", "--cask", "--deps", "foo"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn fetch_formula_and_formulae_aliases_both_set_flag() {
        for argv in [
            ["zapbrew", "fetch", "--formula", "foo"],
            ["zapbrew", "fetch", "--formulae", "foo"],
        ] {
            let Commands::Fetch(args) = command(argv.as_slice()) else {
                panic!("expected fetch");
            };
            assert!(args.formula);
            assert!(!args.cask);
            assert_eq!(args.names, vec!["foo"]);
        }
    }

    #[test]
    fn fetch_cask_and_casks_aliases_both_set_flag() {
        for argv in [
            ["zapbrew", "fetch", "--cask", "foo"],
            ["zapbrew", "fetch", "--casks", "foo"],
        ] {
            let Commands::Fetch(args) = command(argv.as_slice()) else {
                panic!("expected fetch");
            };
            assert!(args.cask);
            assert!(!args.formula);
            assert_eq!(args.names, vec!["foo"]);
        }
    }
}
