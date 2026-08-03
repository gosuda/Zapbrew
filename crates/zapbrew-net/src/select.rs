//! Bottle tag selection using `env.bottle_tag` (never host detection here).

use zapbrew_prefix::Env;
use zapbrew_types::{Arch, BottleFile, BottleTag, FormulaName, MacOsVersion};

use crate::error::NetError;

/// Select a bottle for the host tag stored on `env`.
///
/// Order:
/// 1. Exact `env.bottle_tag`
/// 2. On macOS: older same-arch tags, newest-first (`MacOsVersion` descending)
/// 3. Universal `all`
/// 4. [`NetError::NoBottle`]
pub fn select_bottle<'a>(
    env: &Env,
    name: &FormulaName,
    files: &'a [BottleFile],
) -> Result<&'a BottleFile, NetError> {
    if let Some(file) = files.iter().find(|f| f.tag == env.bottle_tag) {
        return Ok(file);
    }

    if let BottleTag::MacOs { arch, version } = env.bottle_tag
        && let Some(file) = select_older_macos(files, arch, version)
    {
        return Ok(file);
    }

    if let Some(file) = files.iter().find(|f| f.tag == BottleTag::All) {
        return Ok(file);
    }

    Err(NetError::NoBottle {
        name: name.as_str().to_owned(),
        tag: env.bottle_tag,
    })
}

fn select_older_macos(
    files: &[BottleFile],
    arch: Arch,
    host_version: MacOsVersion,
) -> Option<&BottleFile> {
    for &candidate_version in MacOsVersion::ALL.iter().rev() {
        if candidate_version >= host_version {
            continue;
        }
        let want = BottleTag::MacOs {
            arch,
            version: candidate_version,
        };
        if let Some(file) = files.iter().find(|f| f.tag == want) {
            return Some(file);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::str::FromStr;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};
    use zapbrew_types::Checksum;

    struct PanicRunner;
    impl CommandRunner for PanicRunner {
        fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
            panic!("command runner should not be invoked for linux detect_from");
        }
    }

    fn test_env(tag: BottleTag) -> (TempDir, Env) {
        let dir = TempDir::new().expect("tempdir");
        let home = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
        let cache = home.join("cache");
        let mut vars = HashMap::new();
        vars.insert("HOMEBREW_CACHE".to_owned(), cache.as_str().to_owned());
        vars.insert(
            "HOMEBREW_PREFIX".to_owned(),
            home.join("prefix").as_str().to_owned(),
        );
        let input = EnvDetectInput {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            home,
            xdg_cache_home: None,
            vars,
            available_parallelism: 2,
        };
        let mut env = Env::detect_from(&input, &PanicRunner).expect("env");
        env.bottle_tag = tag;
        (dir, env)
    }

    fn bottle(tag: &str, sha: &str) -> BottleFile {
        BottleFile {
            tag: BottleTag::from_str(tag).expect("tag"),
            cellar: "any".into(),
            url: format!("https://example.test/{tag}"),
            sha256: Checksum::from_str(sha).expect("sha"),
        }
    }

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SHA3: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn selects_exact_linux_tag() {
        let (_dir, env) = test_env(BottleTag::Linux { arch: Arch::X86_64 });
        let name = FormulaName::from_str("wget").expect("name");
        let files = vec![
            bottle("arm64_linux", SHA),
            bottle("x86_64_linux", SHA2),
            bottle("all", SHA3),
        ];
        let selected = select_bottle(&env, &name, &files).expect("select");
        assert_eq!(selected.tag, BottleTag::Linux { arch: Arch::X86_64 });
        assert_eq!(selected.sha256.as_str(), SHA2);
    }

    #[test]
    fn falls_back_to_older_macos_descending() {
        let (_dir, env) = test_env(BottleTag::MacOs {
            arch: Arch::Arm64,
            version: MacOsVersion::Sonoma,
        });
        let name = FormulaName::from_str("wget").expect("name");
        let files = vec![
            bottle("arm64_monterey", SHA),
            bottle("arm64_ventura", SHA2),
            bottle("all", SHA3),
        ];
        let selected = select_bottle(&env, &name, &files).expect("select");
        assert_eq!(
            selected.tag,
            BottleTag::MacOs {
                arch: Arch::Arm64,
                version: MacOsVersion::Ventura,
            }
        );
        assert_eq!(selected.sha256.as_str(), SHA2);
    }

    #[test]
    fn falls_back_to_all() {
        let (_dir, env) = test_env(BottleTag::Linux { arch: Arch::X86_64 });
        let name = FormulaName::from_str("wget").expect("name");
        let files = vec![bottle("arm64_linux", SHA), bottle("all", SHA2)];
        let selected = select_bottle(&env, &name, &files).expect("select");
        assert_eq!(selected.tag, BottleTag::All);
    }

    #[test]
    fn no_bottle_message_omits_error_prefix() {
        let (_dir, env) = test_env(BottleTag::Linux { arch: Arch::X86_64 });
        let name = FormulaName::from_str("wget").expect("name");
        let files = vec![bottle("arm64_linux", SHA)];
        let err = select_bottle(&env, &name, &files).expect_err("no bottle");
        let msg = err.to_string();
        assert!(
            !msg.starts_with("Error:"),
            "Reporter owns Error: prefix, got {msg}"
        );
        assert_eq!(
            msg,
            "wget: no bottle available for x86_64_linux. brew can build from source; zapbrew cannot."
        );
    }
}
