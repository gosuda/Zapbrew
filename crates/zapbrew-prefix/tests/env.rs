//! Integration tests for Env detection, typed vars, bottle tags, and shellenv.
//!
//! All host/platform cases go through [`Env::detect_from`] with an explicit
//! [`EnvDetectInput`] so tests never mutate process-global environment.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::process::ExitStatus;
use std::sync::Mutex;

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, PrefixError, ProxyEnv, Shell,
};
use zapbrew_types::{Arch, BottleTag, MacOsVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedCommand {
    program: OsString,
    args: Vec<OsString>,
}

impl From<&CommandSpec> for RecordedCommand {
    fn from(spec: &CommandSpec) -> Self {
        Self {
            program: spec.program().to_os_string(),
            args: spec.arguments().to_vec(),
        }
    }
}

struct RecordingCommandRunner {
    records: Mutex<Vec<RecordedCommand>>,
    response: Result<CommandOutput, io::ErrorKind>,
}

impl RecordingCommandRunner {
    fn ok(stdout: &str) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            response: Ok(CommandOutput::new(
                exit_status(0),
                stdout.as_bytes().to_vec(),
                Vec::new(),
            )),
        }
    }

    fn failed(status: i32, stderr: &str) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            response: Ok(CommandOutput::new(
                exit_status(status),
                Vec::new(),
                stderr.as_bytes().to_vec(),
            )),
        }
    }

    fn take_records(&self) -> Vec<RecordedCommand> {
        match self.records.lock() {
            Ok(mut guard) => {
                let records = guard.clone();
                guard.clear();
                records
            }
            Err(_) => panic!("recording runner lock poisoned"),
        }
    }
}

impl CommandRunner for RecordingCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        match self.records.lock() {
            Ok(mut guard) => guard.push(RecordedCommand::from(spec)),
            Err(_) => return Err(io::Error::other("recording runner lock poisoned")),
        }
        match &self.response {
            Ok(output) => Ok(output.clone()),
            Err(kind) => Err(io::Error::from(*kind)),
        }
    }
}

struct PanicRunner;

impl CommandRunner for PanicRunner {
    fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("Linux detect must not invoke CommandRunner");
    }
}

fn exit_status(code: i32) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }
    #[cfg(not(unix))]
    {
        let _ = code;
        panic!("exit_status helper is only implemented on Unix targets");
    }
}

fn scratch_home() -> (TempDir, Utf8PathBuf) {
    let dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir: {err}"),
    };
    let home = match Utf8PathBuf::from_path_buf(dir.path().to_path_buf()) {
        Ok(path) => path,
        Err(path) => panic!("non-utf8 temp path: {path:?}"),
    };
    (dir, home)
}

fn base_input(os: &str, arch: &str, home: Utf8PathBuf) -> EnvDetectInput {
    EnvDetectInput {
        os: os.to_owned(),
        arch: arch.to_owned(),
        home,
        xdg_cache_home: None,
        vars: HashMap::new(),
        available_parallelism: 4,
    }
}

fn detect_ok(input: &EnvDetectInput, runner: &dyn CommandRunner) -> Env {
    match Env::detect_from(input, runner) {
        Ok(env) => env,
        Err(err) => panic!("detect_from failed: {err}"),
    }
}

#[test]
fn linux_defaults_paths_repository_and_bottle_tag() {
    let (_tmp, home) = scratch_home();
    let input = base_input("linux", "x86_64", home.clone());
    let env = detect_ok(&input, &PanicRunner);

    assert_eq!(env.home, home);
    assert_eq!(env.prefix.as_str(), "/home/linuxbrew/.linuxbrew");
    assert_eq!(env.cellar.as_str(), "/home/linuxbrew/.linuxbrew/Cellar");
    assert_eq!(env.cache, home.join(".cache/Homebrew"));
    assert_eq!(env.logs, home.join(".cache/Homebrew/Logs"));
    assert!(
        env.temp.as_str() == "/var/tmp" || env.temp.as_str() == "/tmp",
        "unexpected linux temp {}",
        env.temp
    );
    assert_eq!(
        env.locks.as_str(),
        "/home/linuxbrew/.linuxbrew/var/homebrew/locks"
    );
    assert_eq!(
        env.pins.as_str(),
        "/home/linuxbrew/.linuxbrew/var/homebrew/pinned"
    );
    assert_eq!(
        env.linked.as_str(),
        "/home/linuxbrew/.linuxbrew/var/homebrew/linked"
    );
    assert_eq!(
        env.repository.as_str(),
        "/home/linuxbrew/.linuxbrew/Homebrew"
    );
    assert_eq!(
        env.library.as_str(),
        "/home/linuxbrew/.linuxbrew/Homebrew/Library"
    );
    assert_eq!(env.caskroom.as_str(), "/home/linuxbrew/.linuxbrew/Caskroom");
    assert_eq!(env.bottle_tag, BottleTag::Linux { arch: Arch::X86_64 });
    assert_eq!(env.download_concurrency, 8);
    assert_eq!(env.api_auto_update_secs, 450);
    assert_eq!(env.cleanup_max_age_days, 120);
    assert_eq!(env.api_domain, "https://formulae.brew.sh/api");
    assert_eq!(env.bottle_domain, "https://ghcr.io/v2/homebrew/core");
    assert_eq!(env.install_badge, "🍺");
    assert_eq!(env.forbidden_owner, "you");
    assert!(!env.no_color);
    assert!(!env.color);
}

#[test]
fn linux_arm_bottle_tag_and_xdg_cache() {
    let (_tmp, home) = scratch_home();
    let xdg = home.join("xdg-cache");
    let mut input = base_input("linux", "aarch64", home);
    input.xdg_cache_home = Some(xdg.clone());
    let env = detect_ok(&input, &PanicRunner);

    assert_eq!(env.cache, xdg.join("Homebrew"));
    assert_eq!(env.logs, xdg.join("Homebrew/Logs"));
    assert_eq!(env.bottle_tag, BottleTag::Linux { arch: Arch::Arm64 });
}

#[test]
fn intel_macos_defaults_and_exact_sw_vers_spec() {
    let (_tmp, home) = scratch_home();
    let input = base_input("macos", "x86_64", home.clone());
    let runner = RecordingCommandRunner::ok("14.5\n");
    let env = detect_ok(&input, &runner);

    assert_eq!(env.prefix.as_str(), "/usr/local");
    assert_eq!(env.cellar.as_str(), "/usr/local/Cellar");
    assert_eq!(env.cache, home.join("Library/Caches/Homebrew"));
    assert_eq!(env.logs, home.join("Library/Logs/Homebrew"));
    assert_eq!(env.temp.as_str(), "/private/tmp");
    assert_eq!(env.repository.as_str(), "/usr/local/Homebrew");
    assert_eq!(env.library.as_str(), "/usr/local/Homebrew/Library");
    assert_eq!(env.caskroom.as_str(), "/usr/local/Caskroom");
    assert_eq!(
        env.bottle_tag,
        BottleTag::MacOs {
            arch: Arch::X86_64,
            version: MacOsVersion::Sonoma,
        }
    );

    let records = runner.take_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].program, OsStr::new("sw_vers"));
    assert_eq!(records[0].args, [OsString::from("-productVersion")]);
}

#[test]
fn arm_macos_defaults_repository_equals_prefix() {
    let (_tmp, home) = scratch_home();
    let input = base_input("macos", "arm64", home.clone());
    let runner = RecordingCommandRunner::ok("15.0");
    let env = detect_ok(&input, &runner);

    assert_eq!(env.prefix.as_str(), "/opt/homebrew");
    assert_eq!(env.repository.as_str(), "/opt/homebrew");
    assert_eq!(env.library.as_str(), "/opt/homebrew/Library");
    assert_eq!(env.caskroom.as_str(), "/opt/homebrew/Caskroom");
    assert_eq!(env.locks.as_str(), "/opt/homebrew/var/homebrew/locks");
    assert_eq!(env.pins.as_str(), "/opt/homebrew/var/homebrew/pinned");
    assert_eq!(env.linked.as_str(), "/opt/homebrew/var/homebrew/linked");
    assert_eq!(
        env.bottle_tag,
        BottleTag::MacOs {
            arch: Arch::Arm64,
            version: MacOsVersion::Sequoia,
        }
    );
    assert_eq!(runner.take_records().len(), 1);
}

#[test]
fn path_and_repository_overrides() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home.clone());
    input
        .vars
        .insert("HOMEBREW_PREFIX".into(), home.join("prefix").into_string());
    input
        .vars
        .insert("HOMEBREW_CELLAR".into(), home.join("cellar").into_string());
    input
        .vars
        .insert("HOMEBREW_CACHE".into(), home.join("cache").into_string());
    input
        .vars
        .insert("HOMEBREW_LOGS".into(), home.join("logs").into_string());
    input
        .vars
        .insert("HOMEBREW_TEMP".into(), home.join("temp").into_string());
    input.vars.insert(
        "HOMEBREW_REPOSITORY".into(),
        home.join("repo").into_string(),
    );

    let env = detect_ok(&input, &PanicRunner);
    assert_eq!(env.prefix, home.join("prefix"));
    assert_eq!(env.cellar, home.join("cellar"));
    assert_eq!(env.cache, home.join("cache"));
    assert_eq!(env.logs, home.join("logs"));
    assert_eq!(env.temp, home.join("temp"));
    assert_eq!(env.repository, home.join("repo"));
    assert_eq!(env.library, home.join("repo/Library"));
    assert_eq!(env.caskroom, home.join("prefix/Caskroom"));
    assert_eq!(env.locks, home.join("prefix/var/homebrew/locks"));
    assert_eq!(env.pins, home.join("prefix/var/homebrew/pinned"));
    assert_eq!(env.linked, home.join("prefix/var/homebrew/linked"));
}

#[test]
fn scalar_list_and_bool_vars() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home);
    input.available_parallelism = 3;
    input.vars.insert(
        "HOMEBREW_API_DOMAIN".into(),
        "https://mirror.example/api".into(),
    );
    input
        .vars
        .insert("HOMEBREW_API_AUTO_UPDATE_SECS".into(), "900".into());
    input.vars.insert(
        "HOMEBREW_BOTTLE_DOMAIN".into(),
        "https://mirror.example/bottles".into(),
    );
    input
        .vars
        .insert("HOMEBREW_NO_AUTO_UPDATE".into(), "1".into());
    input
        .vars
        .insert("HOMEBREW_NO_INSTALL_CLEANUP".into(), "true".into());
    input
        .vars
        .insert("HOMEBREW_NO_AUTOREMOVE".into(), "yes".into());
    input
        .vars
        .insert("HOMEBREW_CLEANUP_MAX_AGE_DAYS".into(), "30".into());
    input
        .vars
        .insert("HOMEBREW_NO_CLEANUP_FORMULAE".into(), "foo, bar baz".into());
    input.vars.insert("HOMEBREW_DEBUG".into(), "1".into());
    input.vars.insert("HOMEBREW_VERBOSE".into(), "1".into());
    input
        .vars
        .insert("HOMEBREW_DOWNLOAD_CONCURRENCY".into(), "5".into());
    input
        .vars
        .insert("HOMEBREW_GITHUB_PACKAGES_TOKEN".into(), "gh-token".into());
    input
        .vars
        .insert("HOMEBREW_DOCKER_REGISTRY_TOKEN".into(), "reg-token".into());
    input.vars.insert("HOMEBREW_NO_EMOJI".into(), "1".into());
    input
        .vars
        .insert("HOMEBREW_INSTALL_BADGE".into(), "OK".into());
    input
        .vars
        .insert("HOMEBREW_NO_ENV_HINTS".into(), "1".into());
    input
        .vars
        .insert("HOMEBREW_NO_INSTALL_UPGRADE".into(), "1".into());
    input
        .vars
        .insert("HOMEBREW_FORBIDDEN_FORMULAE".into(), "a b,c".into());
    input
        .vars
        .insert("HOMEBREW_FORBIDDEN_TAPS".into(), "user/repo".into());
    input
        .vars
        .insert("HOMEBREW_FORBIDDEN_LICENSES".into(), "GPL-3.0, MIT".into());
    input
        .vars
        .insert("HOMEBREW_FORBIDDEN_OWNER".into(), "ops".into());
    input.vars.insert(
        "HOMEBREW_ALLOWED_TAPS".into(),
        "homebrew/core extrahome/tap".into(),
    );

    let env = detect_ok(&input, &PanicRunner);
    assert_eq!(env.api_domain, "https://mirror.example/api");
    assert_eq!(env.api_auto_update_secs, 900);
    assert_eq!(env.bottle_domain, "https://mirror.example/bottles");
    assert!(env.no_auto_update);
    assert!(env.no_install_cleanup);
    assert!(env.no_autoremove);
    assert_eq!(env.cleanup_max_age_days, 30);
    assert_eq!(
        env.no_cleanup_formulae,
        vec!["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]
    );
    assert!(env.debug);
    assert!(env.verbose);
    assert_eq!(env.download_concurrency, 5);
    assert_eq!(env.github_packages_token.as_deref(), Some("gh-token"));
    assert_eq!(env.docker_registry_token.as_deref(), Some("reg-token"));
    assert!(env.no_emoji);
    assert_eq!(env.install_badge, "OK");
    assert!(env.no_env_hints);
    assert!(env.no_install_upgrade);
    assert_eq!(
        env.forbidden_formulae,
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    );
    assert_eq!(env.forbidden_taps, vec!["user/repo".to_owned()]);
    assert_eq!(
        env.forbidden_licenses,
        vec!["GPL-3.0".to_owned(), "MIT".to_owned()]
    );
    assert_eq!(env.forbidden_owner, "ops");
    assert_eq!(
        env.allowed_taps,
        vec!["homebrew/core".to_owned(), "extrahome/tap".to_owned()]
    );
}

#[test]
fn boolean_presence_treats_any_non_empty_as_true() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home);
    // Homebrew `:set` / presence semantics: any non-empty value enables.
    input
        .vars
        .insert("HOMEBREW_NO_AUTO_UPDATE".into(), "0".into());
    input.vars.insert("HOMEBREW_DEBUG".into(), "false".into());
    let env = detect_ok(&input, &PanicRunner);
    assert!(env.no_auto_update);
    assert!(env.debug);

    input
        .vars
        .insert("HOMEBREW_NO_AUTO_UPDATE".into(), String::new());
    input.vars.remove("HOMEBREW_DEBUG");
    let env = detect_ok(&input, &PanicRunner);
    assert!(!env.no_auto_update);
    assert!(!env.debug);
}

#[test]
fn color_precedence_no_color_overrides_homebrew_color() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home);

    input.vars.insert("HOMEBREW_COLOR".into(), "1".into());
    let env = detect_ok(&input, &PanicRunner);
    assert!(env.color);
    assert!(!env.no_color);

    input.vars.insert("HOMEBREW_NO_COLOR".into(), "1".into());
    let env = detect_ok(&input, &PanicRunner);
    assert!(env.no_color);
    assert!(!env.color);

    input.vars.remove("HOMEBREW_NO_COLOR");
    input.vars.insert("NO_COLOR".into(), "1".into());
    let env = detect_ok(&input, &PanicRunner);
    assert!(env.no_color);
    assert!(!env.color);
}

#[test]
fn download_concurrency_auto_saturates_and_minimum_one() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home);
    input.available_parallelism = 0;
    let env = detect_ok(&input, &PanicRunner);
    assert_eq!(env.download_concurrency, 1);

    input.available_parallelism = usize::MAX;
    input
        .vars
        .insert("HOMEBREW_DOWNLOAD_CONCURRENCY".into(), "auto".into());
    let env = detect_ok(&input, &PanicRunner);
    assert_eq!(env.download_concurrency, usize::MAX);

    input
        .vars
        .insert("HOMEBREW_DOWNLOAD_CONCURRENCY".into(), "0".into());
    let env = detect_ok(&input, &PanicRunner);
    assert_eq!(env.download_concurrency, 1);

    input
        .vars
        .insert("HOMEBREW_DOWNLOAD_CONCURRENCY".into(), "-3".into());
    let env = detect_ok(&input, &PanicRunner);
    assert_eq!(env.download_concurrency, 1);

    for malformed in ["x", "1.5", "12abc"] {
        input
            .vars
            .insert("HOMEBREW_DOWNLOAD_CONCURRENCY".into(), malformed.into());
        let env = detect_ok(&input, &PanicRunner);
        assert_eq!(
            env.download_concurrency, 1,
            "malformed HOMEBREW_DOWNLOAD_CONCURRENCY={malformed:?} should default to 1"
        );
    }
}

#[test]
fn invalid_integers_return_typed_errors() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home);
    input
        .vars
        .insert("HOMEBREW_API_AUTO_UPDATE_SECS".into(), "nope".into());
    match Env::detect_from(&input, &PanicRunner) {
        Err(PrefixError::InvalidEnvironment { name, value }) => {
            assert_eq!(name, "HOMEBREW_API_AUTO_UPDATE_SECS");
            assert_eq!(value, "nope");
        }
        other => panic!("expected InvalidEnvironment, got {other:?}"),
    }

    input.vars.remove("HOMEBREW_API_AUTO_UPDATE_SECS");
}

#[test]
fn proxy_env_honors_lower_and_upper_case() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home);
    input
        .vars
        .insert("http_proxy".into(), "http://lower".into());
    input
        .vars
        .insert("HTTPS_PROXY".into(), "https://upper".into());
    input.vars.insert("ALL_PROXY".into(), "socks5://all".into());
    input.vars.insert("ftp_proxy".into(), "ftp://ftp".into());
    input
        .vars
        .insert("NO_PROXY".into(), "localhost,127.0.0.1".into());

    let env = detect_ok(&input, &PanicRunner);
    assert_eq!(
        env.proxy,
        ProxyEnv {
            http_proxy: Some("http://lower".into()),
            https_proxy: Some("https://upper".into()),
            all_proxy: Some("socks5://all".into()),
            ftp_proxy: Some("ftp://ftp".into()),
            no_proxy: Some("localhost,127.0.0.1".into()),
        }
    );
}

#[test]
fn sw_vers_nonzero_maps_to_command_failed() {
    let (_tmp, home) = scratch_home();
    let input = base_input("macos", "arm64", home);
    let runner = RecordingCommandRunner::failed(1, "boom");
    match Env::detect_from(&input, &runner) {
        Err(PrefixError::CommandFailed {
            program,
            status: _,
            stderr,
        }) => {
            assert_eq!(program, "sw_vers");
            assert!(stderr.contains("boom"));
        }
        other => panic!("expected CommandFailed, got {other:?}"),
    }
    let records = runner.take_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].program, OsStr::new("sw_vers"));
    assert_eq!(records[0].args, [OsString::from("-productVersion")]);
}

#[test]
fn shellenv_templates_for_all_shells_under_scratch_paths() {
    let (_tmp, home) = scratch_home();
    let mut input = base_input("linux", "x86_64", home.clone());
    let prefix = home.join("hb");
    let cellar = prefix.join("Cellar");
    let repo = prefix.join("Homebrew");
    input
        .vars
        .insert("HOMEBREW_PREFIX".into(), prefix.clone().into_string());
    input
        .vars
        .insert("HOMEBREW_CELLAR".into(), cellar.clone().into_string());
    input
        .vars
        .insert("HOMEBREW_REPOSITORY".into(), repo.clone().into_string());
    let env = detect_ok(&input, &PanicRunner);

    let p = prefix.as_str();
    let c = cellar.as_str();
    let r = repo.as_str();

    assert_eq!(
        env.shellenv(Shell::Bash),
        format!(
            "export HOMEBREW_PREFIX=\"{p}\";\n\
             export HOMEBREW_CELLAR=\"{c}\";\n\
             export HOMEBREW_REPOSITORY=\"{r}\";\n\
             export PATH=\"{p}/bin:{p}/sbin${{PATH+:$PATH}}\";\n\
             [ -z \"${{MANPATH-}}\" ] || export MANPATH=\":${{MANPATH#:}}\";\n\
             export INFOPATH=\"{p}/share/info:${{INFOPATH:-}}\";\n"
        )
    );

    assert_eq!(
        env.shellenv(Shell::Zsh),
        format!(
            "export HOMEBREW_PREFIX=\"{p}\";\n\
             export HOMEBREW_CELLAR=\"{c}\";\n\
             export HOMEBREW_REPOSITORY=\"{r}\";\n\
             fpath[1,0]=\"{p}/share/zsh/site-functions\";\n\
             export FPATH;\n\
             export PATH=\"{p}/bin:{p}/sbin${{PATH+:$PATH}}\";\n\
             [ -z \"${{MANPATH-}}\" ] || export MANPATH=\":${{MANPATH#:}}\";\n\
             export INFOPATH=\"{p}/share/info:${{INFOPATH:-}}\";\n"
        )
    );

    assert_eq!(
        env.shellenv(Shell::Fish),
        format!(
            "set --global --export HOMEBREW_PREFIX \"{p}\";\n\
             set --global --export HOMEBREW_CELLAR \"{c}\";\n\
             set --global --export HOMEBREW_REPOSITORY \"{r}\";\n\
             fish_add_path --global --move --path \"{p}/bin\" \"{p}/sbin\";\n\
             if test -n \"$MANPATH[1]\"; set --global --export MANPATH '' $MANPATH; end;\n\
             if not set --query INFOPATH; set INFOPATH ''; end; if not contains \"{p}/share/info\" $INFOPATH; set --global --export INFOPATH \"{p}/share/info\" $INFOPATH; end;\n"
        )
    );

    assert_eq!(
        env.shellenv(Shell::Csh),
        format!(
            "setenv HOMEBREW_PREFIX {p};\n\
             setenv HOMEBREW_CELLAR {c};\n\
             setenv HOMEBREW_REPOSITORY {r};\n\
             setenv PATH {p}/bin:{p}/sbin:$PATH;\n\
             test ${{?MANPATH}} -eq 1 && setenv MANPATH :${{MANPATH}};\n\
             setenv INFOPATH {p}/share/info`test ${{?INFOPATH}} -eq 1 && echo :${{INFOPATH}}`;\n"
        )
    );

    assert_eq!(
        env.shellenv(Shell::Pwsh),
        format!(
            "[System.Environment]::SetEnvironmentVariable('HOMEBREW_PREFIX','{p}',[System.EnvironmentVariableTarget]::Process)\n\
             [System.Environment]::SetEnvironmentVariable('HOMEBREW_CELLAR','{c}',[System.EnvironmentVariableTarget]::Process)\n\
             [System.Environment]::SetEnvironmentVariable('HOMEBREW_REPOSITORY','{r}',[System.EnvironmentVariableTarget]::Process)\n\
             [System.Environment]::SetEnvironmentVariable('PATH',$('{p}/bin:{p}/sbin:'+$ENV:PATH),[System.EnvironmentVariableTarget]::Process)\n\
             [System.Environment]::SetEnvironmentVariable('MANPATH',$('{p}/share/man'+$(if(${{ENV:MANPATH}}){{':'+${{ENV:MANPATH}}}})+':'),[System.EnvironmentVariableTarget]::Process)\n\
             [System.Environment]::SetEnvironmentVariable('INFOPATH',$('{p}/share/info'+$(if(${{ENV:INFOPATH}}){{':'+${{ENV:INFOPATH}}}})),[System.EnvironmentVariableTarget]::Process)\n"
        )
    );
}
