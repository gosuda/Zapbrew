//! Host environment detection: paths, typed Homebrew variables, bottle tag,
//! and `shellenv` templates.
//!
//! Production entry points are [`Env::detect`] (live process +
//! [`SystemCommandRunner`]) and [`Env::detect_with`] (live process + injected
//! runner). Platform/env lookup is isolated behind [`EnvDetectInput`] so tests
//! can exercise Linux and macOS defaults without mutating process-global env.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::thread;

use camino::Utf8PathBuf;
use rustix::fs::{Access, access};
use zapbrew_types::BottleTag;

use crate::command::{CommandRunner, CommandSpec, SystemCommandRunner};
use crate::error::PrefixError;

const API_DEFAULT_DOMAIN: &str = "https://formulae.brew.sh/api";
const BOTTLE_DEFAULT_DOMAIN: &str = "https://ghcr.io/v2/homebrew/core";
const DEFAULT_INSTALL_BADGE: &str = "🍺";
const DEFAULT_FORBIDDEN_OWNER: &str = "you";
const DEFAULT_API_AUTO_UPDATE_SECS: u64 = 450;
const DEFAULT_CLEANUP_MAX_AGE_DAYS: u64 = 120;

const MACOS_ARM_PREFIX: &str = "/opt/homebrew";
const MACOS_INTEL_PREFIX: &str = "/usr/local";
const LINUX_PREFIX: &str = "/home/linuxbrew/.linuxbrew";

/// Shells supported by [`Env::shellenv`].
///
/// Selection from `$SHELL` / parent process belongs to later CLI ops — this
/// module only formats a template for an explicit shell argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Csh,
    Pwsh,
}

/// Standard proxy variables honored for reqwest-native downloads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProxyEnv {
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
    pub all_proxy: Option<String>,
    pub ftp_proxy: Option<String>,
    pub no_proxy: Option<String>,
}

/// Process-independent host/env inputs for [`Env::detect_from`].
///
/// Built from the live process by [`Env::detect`] / [`Env::detect_with`]; tests
/// construct instances directly so Linux and macOS layouts never race on
/// global environment state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvDetectInput {
    /// Canonical host OS name accepted by [`BottleTag::from_host`]: `"linux"`
    /// or `"macos"`.
    pub os: String,
    /// Host architecture (`"x86_64"`, `"arm64"`, `"aarch64"`).
    pub arch: String,
    /// Detected home directory (`$HOME`).
    pub home: Utf8PathBuf,
    /// Optional `$XDG_CACHE_HOME` (Linux cache/logs base).
    pub xdg_cache_home: Option<Utf8PathBuf>,
    /// Environment overlay consulted instead of `std::env`. Only keys that
    /// matter to Env need be present.
    pub vars: HashMap<String, String>,
    /// Parallelism used for `HOMEBREW_DOWNLOAD_CONCURRENCY=auto`.
    pub available_parallelism: usize,
}

/// Detected Homebrew prefix layout and typed environment variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub home: Utf8PathBuf,
    pub prefix: Utf8PathBuf,
    pub cellar: Utf8PathBuf,
    pub cache: Utf8PathBuf,
    pub logs: Utf8PathBuf,
    pub temp: Utf8PathBuf,
    pub locks: Utf8PathBuf,
    pub pins: Utf8PathBuf,
    pub linked: Utf8PathBuf,
    pub repository: Utf8PathBuf,
    pub library: Utf8PathBuf,
    pub caskroom: Utf8PathBuf,
    pub bottle_tag: BottleTag,

    pub api_domain: String,
    pub api_auto_update_secs: u64,
    pub bottle_domain: String,
    pub no_auto_update: bool,
    pub no_install_cleanup: bool,
    pub no_autoremove: bool,
    pub cleanup_max_age_days: u64,
    pub no_cleanup_formulae: Vec<String>,
    /// True when `HOMEBREW_NO_COLOR` or `NO_COLOR` is present (non-empty).
    pub no_color: bool,
    /// True when `HOMEBREW_COLOR` is present and not overridden by no-color.
    pub color: bool,
    pub debug: bool,
    pub verbose: bool,
    pub download_concurrency: usize,
    pub github_packages_token: Option<String>,
    pub docker_registry_token: Option<String>,
    pub docker_registry_basic_auth_token: Option<String>,
    pub no_emoji: bool,
    pub install_badge: String,
    pub no_env_hints: bool,
    pub no_install_upgrade: bool,
    pub forbidden_formulae: Vec<String>,
    pub forbidden_taps: Vec<String>,
    pub forbidden_licenses: Vec<String>,
    pub forbidden_owner: String,
    pub allowed_taps: Vec<String>,
    pub proxy: ProxyEnv,
}

impl Env {
    /// Detect Env from the live process environment using
    /// [`SystemCommandRunner`].
    pub fn detect() -> Result<Self, PrefixError> {
        Self::detect_with(&SystemCommandRunner)
    }

    /// Detect Env from the live process environment with an injected runner
    /// (macOS `sw_vers` only).
    pub fn detect_with(runner: &dyn CommandRunner) -> Result<Self, PrefixError> {
        Self::detect_from(&EnvDetectInput::from_process()?, runner)
    }

    /// Detect Env from an explicit host/env overlay.
    ///
    /// This is the race-free seam used by tests to cover Linux, Intel macOS,
    /// and ARM macOS defaults without mutating process-global environment.
    pub fn detect_from(
        input: &EnvDetectInput,
        runner: &dyn CommandRunner,
    ) -> Result<Self, PrefixError> {
        let bottle_tag = detect_bottle_tag(input, runner)?;
        let paths = resolve_paths(input);
        let typed = resolve_typed(input)?;

        Ok(Self {
            home: paths.home,
            prefix: paths.prefix,
            cellar: paths.cellar,
            cache: paths.cache,
            logs: paths.logs,
            temp: paths.temp,
            locks: paths.locks,
            pins: paths.pins,
            linked: paths.linked,
            repository: paths.repository,
            library: paths.library,
            caskroom: paths.caskroom,
            bottle_tag,
            api_domain: typed.api_domain,
            api_auto_update_secs: typed.api_auto_update_secs,
            bottle_domain: typed.bottle_domain,
            no_auto_update: typed.no_auto_update,
            no_install_cleanup: typed.no_install_cleanup,
            no_autoremove: typed.no_autoremove,
            cleanup_max_age_days: typed.cleanup_max_age_days,
            no_cleanup_formulae: typed.no_cleanup_formulae,
            no_color: typed.no_color,
            color: typed.color,
            debug: typed.debug,
            verbose: typed.verbose,
            download_concurrency: typed.download_concurrency,
            github_packages_token: typed.github_packages_token,
            docker_registry_token: typed.docker_registry_token,
            docker_registry_basic_auth_token: typed.docker_registry_basic_auth_token,
            no_emoji: typed.no_emoji,
            install_badge: typed.install_badge,
            no_env_hints: typed.no_env_hints,
            no_install_upgrade: typed.no_install_upgrade,
            forbidden_formulae: typed.forbidden_formulae,
            forbidden_taps: typed.forbidden_taps,
            forbidden_licenses: typed.forbidden_licenses,
            forbidden_owner: typed.forbidden_owner,
            allowed_taps: typed.allowed_taps,
            proxy: typed.proxy,
        })
    }

    /// Emit shellenv setup for `shell` from `.references/brew/.../shellenv.sh`
    /// (portable PATH branch; no `path_helper`).
    pub fn shellenv(&self, shell: Shell) -> String {
        let prefix = self.prefix.as_str();
        let cellar = self.cellar.as_str();
        let repository = self.repository.as_str();

        match shell {
            Shell::Fish => format!(
                "set --global --export HOMEBREW_PREFIX \"{prefix}\";\n\
                 set --global --export HOMEBREW_CELLAR \"{cellar}\";\n\
                 set --global --export HOMEBREW_REPOSITORY \"{repository}\";\n\
                 fish_add_path --global --move --path \"{prefix}/bin\" \"{prefix}/sbin\";\n\
                 if test -n \"$MANPATH[1]\"; set --global --export MANPATH '' $MANPATH; end;\n\
                 if not set --query INFOPATH; set INFOPATH ''; end; if not contains \"{prefix}/share/info\" $INFOPATH; set --global --export INFOPATH \"{prefix}/share/info\" $INFOPATH; end;\n"
            ),
            Shell::Csh => format!(
                "setenv HOMEBREW_PREFIX {prefix};\n\
                 setenv HOMEBREW_CELLAR {cellar};\n\
                 setenv HOMEBREW_REPOSITORY {repository};\n\
                 setenv PATH {prefix}/bin:{prefix}/sbin:$PATH;\n\
                 test ${{?MANPATH}} -eq 1 && setenv MANPATH :${{MANPATH}};\n\
                 setenv INFOPATH {prefix}/share/info`test ${{?INFOPATH}} -eq 1 && echo :${{INFOPATH}}`;\n"
            ),
            Shell::Pwsh => {
                let mut out = String::new();
                out.push_str(&format!(
                    "[System.Environment]::SetEnvironmentVariable('HOMEBREW_PREFIX','{prefix}',[System.EnvironmentVariableTarget]::Process)\n"
                ));
                out.push_str(&format!(
                    "[System.Environment]::SetEnvironmentVariable('HOMEBREW_CELLAR','{cellar}',[System.EnvironmentVariableTarget]::Process)\n"
                ));
                out.push_str(&format!(
                    "[System.Environment]::SetEnvironmentVariable('HOMEBREW_REPOSITORY','{repository}',[System.EnvironmentVariableTarget]::Process)\n"
                ));
                out.push_str(&format!(
                    "[System.Environment]::SetEnvironmentVariable('PATH',$('{prefix}/bin:{prefix}/sbin:'+$ENV:PATH),[System.EnvironmentVariableTarget]::Process)\n"
                ));
                out.push_str(&format!(
                    "[System.Environment]::SetEnvironmentVariable('MANPATH',$('{prefix}/share/man'+$(if(${{ENV:MANPATH}}){{':'+${{ENV:MANPATH}}}})+':'),[System.EnvironmentVariableTarget]::Process)\n"
                ));
                out.push_str(&format!(
                    "[System.Environment]::SetEnvironmentVariable('INFOPATH',$('{prefix}/share/info'+$(if(${{ENV:INFOPATH}}){{':'+${{ENV:INFOPATH}}}})),[System.EnvironmentVariableTarget]::Process)\n"
                ));
                out
            }
            Shell::Zsh => format!(
                "export HOMEBREW_PREFIX=\"{prefix}\";\n\
                 export HOMEBREW_CELLAR=\"{cellar}\";\n\
                 export HOMEBREW_REPOSITORY=\"{repository}\";\n\
                 fpath[1,0]=\"{prefix}/share/zsh/site-functions\";\n\
                 export FPATH;\n\
                 export PATH=\"{prefix}/bin:{prefix}/sbin${{PATH+:$PATH}}\";\n\
                 [ -z \"${{MANPATH-}}\" ] || export MANPATH=\":${{MANPATH#:}}\";\n\
                 export INFOPATH=\"{prefix}/share/info:${{INFOPATH:-}}\";\n"
            ),
            Shell::Bash => format!(
                "export HOMEBREW_PREFIX=\"{prefix}\";\n\
                 export HOMEBREW_CELLAR=\"{cellar}\";\n\
                 export HOMEBREW_REPOSITORY=\"{repository}\";\n\
                 export PATH=\"{prefix}/bin:{prefix}/sbin${{PATH+:$PATH}}\";\n\
                 [ -z \"${{MANPATH-}}\" ] || export MANPATH=\":${{MANPATH#:}}\";\n\
                 export INFOPATH=\"{prefix}/share/info:${{INFOPATH:-}}\";\n"
            ),
        }
    }
}

impl EnvDetectInput {
    /// Snapshot the live process into an [`EnvDetectInput`].
    pub fn from_process() -> Result<Self, PrefixError> {
        let os = match env::consts::OS {
            "linux" => "linux".to_owned(),
            "macos" => "macos".to_owned(),
            other => {
                return Err(PrefixError::UnsupportedHost {
                    os: other.to_owned(),
                    arch: env::consts::ARCH.to_owned(),
                });
            }
        };

        let home = match env::var_os("HOME") {
            Some(value) => utf8_path_from_os("HOME", value)?,
            None => {
                return Err(PrefixError::InvalidEnvironment {
                    name: "HOME",
                    value: String::new(),
                });
            }
        };

        let xdg_cache_home = match env::var_os("XDG_CACHE_HOME") {
            Some(value) if !value.is_empty() => Some(utf8_path_from_os("XDG_CACHE_HOME", value)?),
            _ => None,
        };

        let mut vars = HashMap::new();
        for key in TRACKED_VARS {
            if let Ok(value) = env::var(key) {
                vars.insert((*key).to_owned(), value);
            }
        }

        let available_parallelism = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);

        Ok(Self {
            os,
            arch: env::consts::ARCH.to_owned(),
            home,
            xdg_cache_home,
            vars,
            available_parallelism,
        })
    }
}

const TRACKED_VARS: &[&str] = &[
    "HOMEBREW_PREFIX",
    "HOMEBREW_CELLAR",
    "HOMEBREW_CACHE",
    "HOMEBREW_LOGS",
    "HOMEBREW_TEMP",
    "HOMEBREW_REPOSITORY",
    "HOMEBREW_API_DOMAIN",
    "HOMEBREW_API_AUTO_UPDATE_SECS",
    "HOMEBREW_BOTTLE_DOMAIN",
    "HOMEBREW_NO_AUTO_UPDATE",
    "HOMEBREW_NO_INSTALL_CLEANUP",
    "HOMEBREW_NO_AUTOREMOVE",
    "HOMEBREW_CLEANUP_MAX_AGE_DAYS",
    "HOMEBREW_NO_CLEANUP_FORMULAE",
    "HOMEBREW_NO_COLOR",
    "NO_COLOR",
    "HOMEBREW_COLOR",
    "HOMEBREW_DEBUG",
    "HOMEBREW_VERBOSE",
    "HOMEBREW_DOWNLOAD_CONCURRENCY",
    "HOMEBREW_GITHUB_PACKAGES_TOKEN",
    "HOMEBREW_DOCKER_REGISTRY_TOKEN",
    "HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN",
    "HOMEBREW_NO_EMOJI",
    "HOMEBREW_INSTALL_BADGE",
    "HOMEBREW_NO_ENV_HINTS",
    "HOMEBREW_NO_INSTALL_UPGRADE",
    "HOMEBREW_FORBIDDEN_FORMULAE",
    "HOMEBREW_FORBIDDEN_TAPS",
    "HOMEBREW_FORBIDDEN_LICENSES",
    "HOMEBREW_FORBIDDEN_OWNER",
    "HOMEBREW_ALLOWED_TAPS",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ftp_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "FTP_PROXY",
    "NO_PROXY",
];

#[derive(Debug)]
struct ResolvedPaths {
    home: Utf8PathBuf,
    prefix: Utf8PathBuf,
    cellar: Utf8PathBuf,
    cache: Utf8PathBuf,
    logs: Utf8PathBuf,
    temp: Utf8PathBuf,
    locks: Utf8PathBuf,
    pins: Utf8PathBuf,
    linked: Utf8PathBuf,
    repository: Utf8PathBuf,
    library: Utf8PathBuf,
    caskroom: Utf8PathBuf,
}

#[derive(Debug)]
struct ResolvedTyped {
    api_domain: String,
    api_auto_update_secs: u64,
    bottle_domain: String,
    no_auto_update: bool,
    no_install_cleanup: bool,
    no_autoremove: bool,
    cleanup_max_age_days: u64,
    no_cleanup_formulae: Vec<String>,
    no_color: bool,
    color: bool,
    debug: bool,
    verbose: bool,
    download_concurrency: usize,
    github_packages_token: Option<String>,
    docker_registry_token: Option<String>,
    docker_registry_basic_auth_token: Option<String>,
    no_emoji: bool,
    install_badge: String,
    no_env_hints: bool,
    no_install_upgrade: bool,
    forbidden_formulae: Vec<String>,
    forbidden_taps: Vec<String>,
    forbidden_licenses: Vec<String>,
    forbidden_owner: String,
    allowed_taps: Vec<String>,
    proxy: ProxyEnv,
}

fn detect_bottle_tag(
    input: &EnvDetectInput,
    runner: &dyn CommandRunner,
) -> Result<BottleTag, PrefixError> {
    let macos_version = if input.os == "macos" {
        Some(read_macos_product_version(runner)?)
    } else {
        None
    };

    BottleTag::from_host(&input.os, &input.arch, macos_version.as_deref()).ok_or_else(|| {
        PrefixError::UnsupportedHost {
            os: input.os.clone(),
            arch: input.arch.clone(),
        }
    })
}

fn read_macos_product_version(runner: &dyn CommandRunner) -> Result<String, PrefixError> {
    let spec = CommandSpec::new("sw_vers").arg("-productVersion");
    let output = runner.run(&spec).map_err(|source| PrefixError::CommandIo {
        program: "sw_vers".to_owned(),
        source,
    })?;

    if !output.success() {
        return Err(PrefixError::CommandFailed {
            program: "sw_vers".to_owned(),
            status: format!("{}", output.status()),
            stderr: String::from_utf8_lossy(output.stderr()).into_owned(),
        });
    }

    Ok(String::from_utf8_lossy(output.stdout()).trim().to_owned())
}

fn resolve_paths(input: &EnvDetectInput) -> ResolvedPaths {
    let home = input.home.clone();
    let prefix = path_override(input, "HOMEBREW_PREFIX")
        .unwrap_or_else(|| default_prefix(&input.os, &input.arch));

    let cellar = path_override(input, "HOMEBREW_CELLAR").unwrap_or_else(|| prefix.join("Cellar"));

    let cache = path_override(input, "HOMEBREW_CACHE").unwrap_or_else(|| {
        if input.os == "macos" {
            home.join("Library/Caches/Homebrew")
        } else {
            linux_cache_home(input).join("Homebrew")
        }
    });

    let logs = path_override(input, "HOMEBREW_LOGS").unwrap_or_else(|| {
        if input.os == "macos" {
            home.join("Library/Logs/Homebrew")
        } else {
            linux_cache_home(input).join("Homebrew/Logs")
        }
    });

    let temp = path_override(input, "HOMEBREW_TEMP").unwrap_or_else(|| default_temp(&input.os));

    let locks = prefix.join("var/homebrew/locks");
    let pins = prefix.join("var/homebrew/pinned");
    let linked = prefix.join("var/homebrew/linked");

    let repository = path_override(input, "HOMEBREW_REPOSITORY").unwrap_or_else(|| {
        if is_apple_silicon(&input.os, &input.arch) {
            prefix.clone()
        } else {
            prefix.join("Homebrew")
        }
    });

    let library = repository.join("Library");
    let caskroom = prefix.join("Caskroom");

    ResolvedPaths {
        home,
        prefix,
        cellar,
        cache,
        logs,
        temp,
        locks,
        pins,
        linked,
        repository,
        library,
        caskroom,
    }
}

fn resolve_typed(input: &EnvDetectInput) -> Result<ResolvedTyped, PrefixError> {
    let api_domain = string_or_default(input, "HOMEBREW_API_DOMAIN", API_DEFAULT_DOMAIN);
    let bottle_domain = string_or_default(input, "HOMEBREW_BOTTLE_DOMAIN", BOTTLE_DEFAULT_DOMAIN);
    let api_auto_update_secs = parse_u64_or_default(
        input,
        "HOMEBREW_API_AUTO_UPDATE_SECS",
        DEFAULT_API_AUTO_UPDATE_SECS,
    )?;
    let cleanup_max_age_days = parse_u64_or_default(
        input,
        "HOMEBREW_CLEANUP_MAX_AGE_DAYS",
        DEFAULT_CLEANUP_MAX_AGE_DAYS,
    )?;
    let download_concurrency = parse_download_concurrency(input);

    let no_color = presence(input, "HOMEBREW_NO_COLOR") || presence(input, "NO_COLOR");
    let color = presence(input, "HOMEBREW_COLOR") && !no_color;

    Ok(ResolvedTyped {
        api_domain,
        api_auto_update_secs,
        bottle_domain,
        no_auto_update: presence(input, "HOMEBREW_NO_AUTO_UPDATE"),
        no_install_cleanup: presence(input, "HOMEBREW_NO_INSTALL_CLEANUP"),
        no_autoremove: presence(input, "HOMEBREW_NO_AUTOREMOVE"),
        cleanup_max_age_days,
        no_cleanup_formulae: parse_list(input, "HOMEBREW_NO_CLEANUP_FORMULAE"),
        no_color,
        color,
        debug: presence(input, "HOMEBREW_DEBUG"),
        verbose: presence(input, "HOMEBREW_VERBOSE"),
        download_concurrency,
        github_packages_token: optional_string(input, "HOMEBREW_GITHUB_PACKAGES_TOKEN"),
        docker_registry_token: optional_string(input, "HOMEBREW_DOCKER_REGISTRY_TOKEN"),
        docker_registry_basic_auth_token: optional_string(
            input,
            "HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN",
        ),
        no_emoji: presence(input, "HOMEBREW_NO_EMOJI"),
        install_badge: string_or_default(input, "HOMEBREW_INSTALL_BADGE", DEFAULT_INSTALL_BADGE),
        no_env_hints: presence(input, "HOMEBREW_NO_ENV_HINTS"),
        no_install_upgrade: presence(input, "HOMEBREW_NO_INSTALL_UPGRADE"),
        forbidden_formulae: parse_list(input, "HOMEBREW_FORBIDDEN_FORMULAE"),
        forbidden_taps: parse_list(input, "HOMEBREW_FORBIDDEN_TAPS"),
        forbidden_licenses: parse_list(input, "HOMEBREW_FORBIDDEN_LICENSES"),
        forbidden_owner: string_or_default(
            input,
            "HOMEBREW_FORBIDDEN_OWNER",
            DEFAULT_FORBIDDEN_OWNER,
        ),
        allowed_taps: parse_list(input, "HOMEBREW_ALLOWED_TAPS"),
        proxy: resolve_proxy(input),
    })
}

fn resolve_proxy(input: &EnvDetectInput) -> ProxyEnv {
    ProxyEnv {
        http_proxy: proxy_var(input, "http_proxy", "HTTP_PROXY"),
        https_proxy: proxy_var(input, "https_proxy", "HTTPS_PROXY"),
        all_proxy: proxy_var(input, "all_proxy", "ALL_PROXY"),
        ftp_proxy: proxy_var(input, "ftp_proxy", "FTP_PROXY"),
        no_proxy: proxy_var(input, "no_proxy", "NO_PROXY"),
    }
}

fn proxy_var(input: &EnvDetectInput, lower: &str, upper: &str) -> Option<String> {
    optional_string(input, lower).or_else(|| optional_string(input, upper))
}

fn default_prefix(os: &str, arch: &str) -> Utf8PathBuf {
    if is_apple_silicon(os, arch) {
        Utf8PathBuf::from(MACOS_ARM_PREFIX)
    } else if os == "macos" {
        Utf8PathBuf::from(MACOS_INTEL_PREFIX)
    } else {
        Utf8PathBuf::from(LINUX_PREFIX)
    }
}

fn default_temp(os: &str) -> Utf8PathBuf {
    if os == "macos" {
        Utf8PathBuf::from("/private/tmp")
    } else {
        let var_tmp = Path::new("/var/tmp");
        if var_tmp.is_dir() && access(var_tmp, Access::READ_OK | Access::WRITE_OK).is_ok() {
            Utf8PathBuf::from("/var/tmp")
        } else {
            Utf8PathBuf::from("/tmp")
        }
    }
}

fn linux_cache_home(input: &EnvDetectInput) -> Utf8PathBuf {
    input
        .xdg_cache_home
        .clone()
        .unwrap_or_else(|| input.home.join(".cache"))
}

fn is_apple_silicon(os: &str, arch: &str) -> bool {
    os == "macos" && matches!(arch, "arm64" | "aarch64")
}

fn path_override(input: &EnvDetectInput, key: &str) -> Option<Utf8PathBuf> {
    optional_string(input, key).map(Utf8PathBuf::from)
}

fn presence(input: &EnvDetectInput, key: &str) -> bool {
    match input.vars.get(key) {
        Some(value) => !value.is_empty(),
        None => false,
    }
}

fn optional_string(input: &EnvDetectInput, key: &str) -> Option<String> {
    match input.vars.get(key) {
        Some(value) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

fn string_or_default(input: &EnvDetectInput, key: &str, default: &str) -> String {
    optional_string(input, key).unwrap_or_else(|| default.to_owned())
}

fn parse_u64_or_default(
    input: &EnvDetectInput,
    key: &'static str,
    default: u64,
) -> Result<u64, PrefixError> {
    match optional_string(input, key) {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| PrefixError::InvalidEnvironment { name: key, value }),
    }
}

fn parse_download_concurrency(input: &EnvDetectInput) -> usize {
    match optional_string(input, "HOMEBREW_DOWNLOAD_CONCURRENCY") {
        None => auto_download_concurrency(input.available_parallelism),
        Some(value) if value == "auto" => auto_download_concurrency(input.available_parallelism),
        Some(value) => value
            .parse::<i64>()
            .ok()
            .and_then(|parsed| usize::try_from(parsed).ok())
            .map(|parsed| parsed.max(1))
            .unwrap_or(1),
    }
}

fn auto_download_concurrency(parallelism: usize) -> usize {
    parallelism.saturating_mul(2).max(1)
}

fn parse_list(input: &EnvDetectInput, key: &str) -> Vec<String> {
    match optional_string(input, key) {
        None => Vec::new(),
        Some(value) => value
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}

fn utf8_path_from_os(
    name: &'static str,
    value: impl Into<PathBuf>,
) -> Result<Utf8PathBuf, PrefixError> {
    let path = value.into();
    Utf8PathBuf::from_path_buf(path).map_err(|path| PrefixError::InvalidEnvironment {
        name,
        value: path.to_string_lossy().into_owned(),
    })
}
