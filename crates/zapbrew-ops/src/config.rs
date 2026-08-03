use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;

use jiff::Timestamp;
use zapbrew_prefix::CommandSpec;

use crate::{Ctx, OpError};

const DEFAULT_API_DOMAIN: &str = "https://formulae.brew.sh/api";
const DEFAULT_BOTTLE_DOMAIN: &str = "https://ghcr.io/v2/homebrew/core";
const DEFAULT_API_AUTO_UPDATE_SECS: &str = "450";
const DEFAULT_CLEANUP_MAX_AGE_DAYS: &str = "120";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Args;

pub async fn run(ctx: &Ctx, _args: Args) -> Result<(), OpError> {
    let vars = process_homebrew_vars();
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    for line in lines(ctx, &vars, cores) {
        ctx.reporter.print(&line);
    }
    Ok(())
}

pub(crate) fn lines(ctx: &Ctx, vars: &BTreeMap<String, String>, cores: usize) -> Vec<String> {
    let repository = ctx.env.repository.as_std_path();
    let origin = command_text(
        ctx,
        CommandSpec::new("git")
            .arg("-C")
            .arg(repository)
            .args(["remote", "get-url", "origin"]),
        "(none)",
    );
    let origin = if contains_credentials(&origin) {
        "set".to_owned()
    } else {
        origin
    };
    let head = command_text(
        ctx,
        CommandSpec::new("git")
            .arg("-C")
            .arg(repository)
            .args(["rev-parse", "HEAD"]),
        "(none)",
    );
    let last_commit = command_text(
        ctx,
        CommandSpec::new("git")
            .arg("-C")
            .arg(repository)
            .args(["log", "-1", "--format=%cd"]),
        "never",
    );
    let git = tool_description(ctx, "git", &["--version"], "git version ");
    let curl = tool_description(ctx, "curl", &["--version"], "curl ");

    let mut output = vec![
        format!(
            "HOMEBREW_VERSION: zapbrew {} (Homebrew 5-compatible)",
            env!("CARGO_PKG_VERSION")
        ),
        format!("ORIGIN: {origin}"),
        format!("HEAD: {head}"),
        format!("Last commit: {last_commit}"),
        core_json_line(ctx),
        format!("HOMEBREW_PREFIX: {}", ctx.env.prefix),
    ];

    let default_cellar = ctx.env.prefix.join("Cellar");
    if ctx.env.cellar != default_cellar {
        output.push(format!("HOMEBREW_CELLAR: {}", ctx.env.cellar));
    }
    for (key, value) in vars {
        if !key.starts_with("HOMEBREW_")
            || is_pinned_key(key)
            || is_default_value(key, value)
            || value.is_empty()
        {
            continue;
        }
        let rendered = if is_sensitive(key, value) || is_boolean_key(key) {
            "set"
        } else {
            value
        };
        output.push(format!("{key}: {rendered}"));
    }

    output.extend([
        format!("Rust: {}", env!("ZAPBREW_RUSTC_VERSION")),
        format!(
            "CPU: {cores}-core {}-bit {}",
            usize::BITS,
            env::consts::ARCH
        ),
        format!("Git: {git}"),
        format!("Curl: {curl}"),
    ]);
    output
}

fn process_homebrew_vars() -> BTreeMap<String, String> {
    env::vars_os()
        .filter_map(|(key, value)| homebrew_var(key, value))
        .collect()
}

fn homebrew_var(key: OsString, value: OsString) -> Option<(String, String)> {
    let key = key.into_string().ok()?;
    if !key.starts_with("HOMEBREW_") {
        return None;
    }
    let value = match value.into_string() {
        Ok(value) => value,
        Err(_) => "set".to_owned(),
    };
    Some((key, value))
}

fn is_pinned_key(key: &str) -> bool {
    matches!(
        key,
        "HOMEBREW_VERSION" | "HOMEBREW_PREFIX" | "HOMEBREW_CELLAR"
    )
}

fn is_default_value(key: &str, value: &str) -> bool {
    matches!(
        (key, value),
        ("HOMEBREW_API_DOMAIN", DEFAULT_API_DOMAIN)
            | ("HOMEBREW_BOTTLE_DOMAIN", DEFAULT_BOTTLE_DOMAIN)
            | (
                "HOMEBREW_API_AUTO_UPDATE_SECS",
                DEFAULT_API_AUTO_UPDATE_SECS
            )
            | (
                "HOMEBREW_CLEANUP_MAX_AGE_DAYS",
                DEFAULT_CLEANUP_MAX_AGE_DAYS
            )
    )
}

fn is_boolean_key(key: &str) -> bool {
    matches!(
        key,
        "HOMEBREW_COLOR"
            | "HOMEBREW_DEBUG"
            | "HOMEBREW_NO_AUTO_UPDATE"
            | "HOMEBREW_NO_AUTOREMOVE"
            | "HOMEBREW_NO_COLOR"
            | "HOMEBREW_NO_EMOJI"
            | "HOMEBREW_NO_ENV_HINTS"
            | "HOMEBREW_NO_INSTALL_CLEANUP"
            | "HOMEBREW_NO_INSTALL_UPGRADE"
            | "HOMEBREW_VERBOSE"
    )
}

fn is_sensitive(key: &str, value: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "SECRET",
        "CREDENTIAL",
        "AUTH",
        "KEY",
        "PROXY",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
        || contains_credentials(value)
}

fn contains_credentials(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let credential_parameter = [
        "access_key=",
        "api_key=",
        "apikey=",
        "auth=",
        "credential=",
        "password=",
        "passwd=",
        "secret=",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if credential_parameter {
        return true;
    }
    if value.split_once("://").is_some_and(|(_, rest)| {
        rest.split('/')
            .next()
            .is_some_and(|authority| authority.contains('@'))
    }) {
        return true;
    }
    scp_style_credentials(value)
}

fn scp_style_credentials(value: &str) -> bool {
    if value.contains("://") {
        return false;
    }
    let Some((userinfo, host_path)) = value.split_once('@') else {
        return false;
    };
    if userinfo.is_empty() || userinfo.contains('/') {
        return false;
    }
    let Some((host, _path)) = host_path.split_once(':') else {
        return false;
    };
    if host.is_empty() || host.contains('/') {
        return false;
    }
    !is_known_transport_user(userinfo)
}

fn is_known_transport_user(userinfo: &str) -> bool {
    matches!(userinfo, "git" | "ssh" | "hg" | "svn")
}

fn command_text(ctx: &Ctx, spec: CommandSpec, fallback: &str) -> String {
    let Ok(output) = ctx.commands.run(&spec) else {
        return fallback.to_owned();
    };
    if !output.success() {
        return fallback.to_owned();
    }
    let text = String::from_utf8_lossy(output.stdout());
    let text = text.trim();
    if text.is_empty() {
        fallback.to_owned()
    } else {
        text.to_owned()
    }
}

fn tool_description(ctx: &Ctx, program: &str, args: &[&str], prefix: &str) -> String {
    let text = command_text(
        ctx,
        CommandSpec::new(program).args(args.iter().copied()),
        "N/A",
    );
    let Some(first_line) = text.lines().next() else {
        return "N/A".to_owned();
    };
    let Some(version) = first_line
        .strip_prefix(prefix)
        .and_then(|rest| rest.split_whitespace().next())
    else {
        return "N/A".to_owned();
    };
    format!("{version} => {program}")
}

fn core_json_line(ctx: &Ctx) -> String {
    let Ok(cache_metadata) = fs::symlink_metadata(&ctx.env.cache) else {
        return "Core tap: N/A".to_owned();
    };
    if !cache_metadata.is_dir() || cache_metadata.file_type().is_symlink() {
        return "Core tap: N/A".to_owned();
    }
    let api = ctx.env.cache.join("api");
    let Ok(api_metadata) = fs::symlink_metadata(&api) else {
        return "Core tap: N/A".to_owned();
    };
    if !api_metadata.is_dir() || api_metadata.file_type().is_symlink() {
        return "Core tap: N/A".to_owned();
    }
    let path = api.join("formula.jws.json");
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return "Core tap: N/A".to_owned();
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return "Core tap: N/A".to_owned();
    }
    let Ok(timestamp) = Timestamp::from_second(metadata.mtime()) else {
        return "Core tap: N/A".to_owned();
    };
    format!("Core tap JSON: {}", timestamp.strftime("%d %b %H:%M UTC"))
}
