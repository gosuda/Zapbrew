//! Content-addressed cache paths, alias symlinks, checksum verification, and
//! atomic publish for bottle downloads.
//!
//! Layout (mirrors brew's `downloads/` cache):
//! - final file: `$HOMEBREW_CACHE/downloads/<sha256(url)>--<basename>`
//! - in-flight:  `<final>.incomplete` (resumable partial)
//! - alias:      `$HOMEBREW_CACHE/<basename>` -> relative `downloads/<file>`

use std::io::Read;
use std::path::{Component, Path};
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};
use zapbrew_prefix::Env;
use zapbrew_types::{BottleFile, BottleTag, Checksum, FormulaName, PkgVersion};

use crate::error::NetError;

/// Compute the cache layout for a non-GHCR artifact URL with a
/// request-specific alias basename. The final path remains content-addressed by
/// the URL, while the alias uses the caller-supplied `alias_name`.
pub(crate) fn artifact_cache_paths_with_alias(
    env: &Env,
    url: &str,
    alias_name: &str,
) -> Result<CachePaths, NetError> {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let basename = path.rsplit('/').next().unwrap_or_default();
    validate_segment("artifact", basename)?;
    validate_segment("alias", alias_name)?;

    let url_hash = hex_sha256(url.as_bytes());
    let hashed_name = format!("{url_hash}--{basename}");
    validate_download_name(&hashed_name, &url_hash, basename)?;

    let final_path = env.cache.join("downloads").join(&hashed_name);
    Ok(CachePaths {
        incomplete: incomplete_path(&final_path),
        final_path,
        alias: env.cache.join(alias_name),
        relative_target: relative_alias_target(&hashed_name),
    })
}

/// Length of a lowercase-hex SHA-256 digest.
const HASH_HEX_LEN: usize = 64;

/// Where one bottle download lives in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CachePaths {
    /// Content-addressed final path: `$CACHE/downloads/<sha256(url)>--<basename>`.
    pub(crate) final_path: Utf8PathBuf,
    /// In-progress partial file: `<final_path>.incomplete`.
    pub(crate) incomplete: Utf8PathBuf,
    /// Friendly alias path: `$CACHE/<basename>`.
    pub(crate) alias: Utf8PathBuf,
    /// Relative symlink target for `alias`: `downloads/<hashed_name>`.
    pub(crate) relative_target: String,
}

/// Build the Homebrew bottle basename:
/// `<name>--<pkg_version>.<tag>.bottle[.<rebuild>].tar.gz`.
pub(crate) fn bottle_basename(
    name: &FormulaName,
    pkg_version: &PkgVersion,
    tag: BottleTag,
    rebuild: u32,
) -> Result<String, NetError> {
    let name_seg = name.name();
    let version_seg = pkg_version.to_string();
    let tag_seg = tag.to_string();

    validate_segment("formula", name_seg)?;
    validate_segment("version", &version_seg)?;
    validate_segment("tag", &tag_seg)?;

    let rebuild_part = if rebuild > 0 {
        format!(".{rebuild}")
    } else {
        String::new()
    };
    let basename = format!("{name_seg}--{version_seg}.{tag_seg}.bottle{rebuild_part}.tar.gz");
    validate_segment("bottle", &basename)?;
    Ok(basename)
}

/// Compute the full `CachePaths` layout for one bottle download.
///
/// Every variable path component is validated: the URL-hash prefix must be
/// exactly 64 lowercase hex digits, the basename suffix must be exactly the
/// canonical `bottle_basename` output, and the whole `downloads/<hashed_name>`
/// relative path must be built from single normal segments only.
pub(crate) fn cache_paths(
    env: &Env,
    name: &FormulaName,
    bottle: &BottleFile,
    pkg_version: &PkgVersion,
    rebuild: u32,
) -> Result<CachePaths, NetError> {
    let basename = bottle_basename(name, pkg_version, bottle.tag, rebuild)?;
    let url_hash = hex_sha256(bottle.url.as_bytes());
    let hashed_name = format!("{url_hash}--{basename}");
    validate_download_name(&hashed_name, &url_hash, &basename)?;

    let downloads = env.cache.join("downloads");
    let final_path = downloads.join(&hashed_name);
    let incomplete = incomplete_path(&final_path);
    let alias = env.cache.join(&basename);
    let relative_target = relative_alias_target(&hashed_name);
    Ok(CachePaths {
        final_path,
        incomplete,
        alias,
        relative_target,
    })
}

/// Validate the pieces that make up a `downloads/<hashed_name>` entry:
/// `url_hash` is exactly 64 lowercase hex digits, `hashed_name` is exactly
/// `{url_hash}--{basename}` (the canonical basename, no substitution), and
/// the whole name is a single normal path segment.
pub(crate) fn validate_download_name(
    hashed_name: &str,
    url_hash: &str,
    basename: &str,
) -> Result<(), NetError> {
    if url_hash.len() != HASH_HEX_LEN
        || !url_hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(NetError::InvalidPathSegment {
            kind: "hash",
            value: url_hash.to_owned(),
        });
    }
    let expected = format!("{url_hash}--{basename}");
    if hashed_name != expected {
        return Err(NetError::InvalidPathSegment {
            kind: "download",
            value: hashed_name.to_owned(),
        });
    }
    validate_segment("download", hashed_name)
}

pub(crate) fn incomplete_path(path: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{path}.incomplete"))
}

/// Relative alias target `downloads/<hashed_name>` from `$CACHE`.
pub(crate) fn relative_alias_target(hashed_name: &str) -> String {
    format!("downloads/{hashed_name}")
}

pub(crate) fn validate_segment(kind: &'static str, value: &str) -> Result<(), NetError> {
    let mut components = Path::new(value).components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        Ok(())
    } else {
        Err(NetError::InvalidPathSegment {
            kind,
            value: value.to_owned(),
        })
    }
}

/// Stream a file through SHA-256 in 64 KiB chunks and return its digest.
///
/// Bottles can be tens of MiB; this never loads the file into memory.
pub(crate) fn checksum_file(path: &Utf8Path) -> Result<Checksum, NetError> {
    let mut file =
        std::fs::File::open(path).map_err(|source| NetError::io("open", path, source))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|source| NetError::io("read", path, source))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex = hex_encode(&hasher.finalize());
    // `hex_encode` emits exactly 64 lowercase hex digits, which `Checksum`
    // accepts by construction, so this cannot fail.
    Ok(Checksum::from_str(&hex).expect("sha256 hex is 64 lowercase hex digits"))
}

/// Durably publish a finished `.incomplete` download into the cache.
///
/// 1. `fsync` the incomplete file so the data is on disk before the rename.
/// 2. Atomically rename it to its content-addressed final name.
/// 3. Create or refresh the friendly alias (idempotent relative symlink).
/// 4. `fsync` the affected directories so the rename and symlink are durable.
pub(crate) fn publish(paths: &CachePaths) -> Result<(), NetError> {
    let file = std::fs::File::open(&paths.incomplete)
        .map_err(|source| NetError::io("open", &paths.incomplete, source))?;
    file.sync_all()
        .map_err(|source| NetError::io("sync", &paths.incomplete, source))?;

    if let Some(parent) = paths.final_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| NetError::io("create", parent, source))?;
    }
    std::fs::rename(&paths.incomplete, &paths.final_path)
        .map_err(|source| NetError::io("rename", &paths.final_path, source))?;

    ensure_alias(&paths.alias, &paths.relative_target)?;
    sync_parent_dir(&paths.final_path)?;
    sync_parent_dir(&paths.alias)
}

/// `fsync` a path's parent directory so a completed rename/symlink survives
/// a crash. Opening a directory read-only is fine on Unix.
#[cfg(unix)]
fn sync_parent_dir(path: &Utf8Path) -> Result<(), NetError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let dir = std::fs::File::open(parent).map_err(|source| NetError::io("open", parent, source))?;
    dir.sync_all()
        .map_err(|source| NetError::io("sync", parent, source))
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Utf8Path) -> Result<(), NetError> {
    Ok(())
}

/// Create (or refresh) the friendly alias symlink at `alias` pointing at the
/// relative `relative_target`. Idempotent: a symlink that already points at
/// the target is left untouched; anything else occupying the path is replaced
/// (matching brew's `ln -sf` semantics for the cache).
#[cfg(unix)]
pub(crate) fn ensure_alias(alias: &Utf8Path, relative_target: &str) -> Result<(), NetError> {
    use std::os::unix::fs::symlink;

    if let Ok(meta) = std::fs::symlink_metadata(alias) {
        if meta.file_type().is_symlink()
            && std::fs::read_link(alias).ok().as_deref() == Some(Path::new(relative_target))
        {
            return Ok(());
        }
        std::fs::remove_file(alias).map_err(|source| NetError::io("remove", alias, source))?;
    }
    symlink(relative_target, alias).map_err(|source| NetError::io("symlink", alias, source))
}

/// Non-Unix fallback: symlinks need privileges on Windows, so copy the
/// published file to the alias path instead. Homebrew only runs on Unix;
/// this keeps the crate compilable for any target.
#[cfg(not(unix))]
pub(crate) fn ensure_alias(alias: &Utf8Path, relative_target: &str) -> Result<(), NetError> {
    let Some(parent) = alias.parent() else {
        return Err(NetError::io(
            "copy",
            alias,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "alias path has no parent"),
        ));
    };
    std::fs::copy(parent.join(relative_target), alias)
        .map_err(|source| NetError::io("copy", alias, source))?;
    Ok(())
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_encode(&digest)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use tempfile::TempDir;
    use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};

    struct PanicRunner;
    impl CommandRunner for PanicRunner {
        fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
            panic!("command runner should not be invoked for linux detect_from");
        }
    }

    fn test_env() -> (TempDir, Env) {
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
        let env = Env::detect_from(&input, &PanicRunner).expect("env");
        (dir, env)
    }

    fn bottle(url: &str) -> BottleFile {
        BottleFile {
            tag: BottleTag::from_str("x86_64_linux").expect("tag"),
            cellar: "any".into(),
            url: url.to_owned(),
            sha256: Checksum::from_str(&"a".repeat(64)).expect("sha"),
        }
    }

    const EXAMPLE_URL: &str = "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:0123456789abcdef";

    #[test]
    fn bottle_basename_omits_zero_rebuild() {
        let name = FormulaName::from_str("wget").expect("name");
        let ver = PkgVersion::from_str("1.25.0").expect("ver");
        let tag = BottleTag::from_str("arm64_sonoma").expect("tag");
        let base = bottle_basename(&name, &ver, tag, 0).expect("basename");
        assert_eq!(base, "wget--1.25.0.arm64_sonoma.bottle.tar.gz");
    }

    #[test]
    fn bottle_basename_includes_positive_rebuild() {
        let name = FormulaName::from_str("wget").expect("name");
        let ver = PkgVersion::from_str("1.25.0").expect("ver");
        let tag = BottleTag::from_str("x86_64_linux").expect("tag");
        let base = bottle_basename(&name, &ver, tag, 2).expect("basename");
        assert_eq!(base, "wget--1.25.0.x86_64_linux.bottle.2.tar.gz");
    }

    #[test]
    fn rejects_traversal_segments() {
        for (kind, value) in [
            ("formula", "../etc"),
            ("formula", "a/b"),
            ("formula", "a/b/c"),
            ("formula", "/etc/passwd"),
            ("formula", ""),
            ("formula", "."),
            ("formula", ".."),
        ] {
            assert!(
                validate_segment(kind, value).is_err(),
                "segment {value:?} should be rejected"
            );
        }
    }

    #[test]
    fn unsafe_version_segment_is_rejected() {
        // PkgVersion parsing is permissive; the basename builder must reject
        // a version whose display form is not a single safe path segment.
        let name = FormulaName::from_str("wget").expect("name");
        let ver = PkgVersion::from_str("1.0/../evil").expect("parses");
        let tag = BottleTag::from_str("x86_64_linux").expect("tag");
        let err = bottle_basename(&name, &ver, tag, 0).expect_err("unsafe version");
        assert!(err.to_string().contains("not a safe path segment"));
    }

    #[test]
    fn cache_paths_layout_matches_brew() {
        let (_dir, env) = test_env();
        let name = FormulaName::from_str("wget").expect("name");
        let ver = PkgVersion::from_str("1.25.0").expect("ver");
        let file = bottle(EXAMPLE_URL);
        let paths = cache_paths(&env, &name, &file, &ver, 0).expect("paths");

        let basename = "wget--1.25.0.x86_64_linux.bottle.tar.gz";
        let url_hash = hex_sha256(EXAMPLE_URL.as_bytes());
        let hashed_name = format!("{url_hash}--{basename}");
        assert_eq!(
            paths.final_path,
            env.cache.join("downloads").join(&hashed_name)
        );
        assert_eq!(
            paths.incomplete,
            env.cache
                .join("downloads")
                .join(format!("{hashed_name}.incomplete"))
        );
        assert_eq!(paths.alias, env.cache.join(basename));
        assert_eq!(paths.relative_target, format!("downloads/{hashed_name}"));
        assert_eq!(url_hash.len(), HASH_HEX_LEN);
        assert!(url_hash.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn download_name_validation_accepts_canonical_form() {
        let basename = "wget--1.25.0.x86_64_linux.bottle.tar.gz";
        let url_hash = hex_sha256(EXAMPLE_URL.as_bytes());
        let hashed_name = format!("{url_hash}--{basename}");
        validate_download_name(&hashed_name, &url_hash, basename).expect("canonical form");
    }

    #[test]
    fn download_name_validation_rejects_bad_hash() {
        let basename = "wget--1.25.0.x86_64_linux.bottle.tar.gz";
        let good = hex_sha256(EXAMPLE_URL.as_bytes());
        for bad in [&good.to_uppercase()[..], &good[..63], &"g".repeat(64)[..]] {
            let hashed_name = format!("{bad}--{basename}");
            assert!(
                validate_download_name(&hashed_name, bad, basename).is_err(),
                "hash {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn download_name_validation_requires_exact_basename() {
        let url_hash = hex_sha256(EXAMPLE_URL.as_bytes());
        let hashed_name = format!("{url_hash}--other--2.0.x86_64_linux.bottle.tar.gz");
        // A basename that does not match the suffix embedded in `hashed_name`
        // is rejected, even though it is a well-formed segment itself.
        assert!(
            validate_download_name(
                &hashed_name,
                &url_hash,
                "wget--1.25.0.x86_64_linux.bottle.tar.gz"
            )
            .is_err()
        );
        // The exact canonical basename for this hashed_name is accepted.
        assert!(
            validate_download_name(
                &hashed_name,
                &url_hash,
                "other--2.0.x86_64_linux.bottle.tar.gz"
            )
            .is_ok()
        );
    }

    #[test]
    fn download_name_validation_rejects_traversal() {
        let url_hash = hex_sha256(EXAMPLE_URL.as_bytes());
        let hashed_name = format!("{url_hash}--../../etc/passwd");
        assert!(validate_download_name(&hashed_name, &url_hash, "../../etc/passwd").is_err());
    }

    #[test]
    fn checksum_file_streams_sha256() {
        let dir = TempDir::new().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf())
            .expect("utf8")
            .join("blob");

        std::fs::write(&path, b"the quick brown fox").expect("write");
        let digest = checksum_file(&path).expect("checksum");
        assert_eq!(digest.as_str(), hex_sha256(b"the quick brown fox"));

        // Large enough to cross several 64 KiB chunk boundaries.
        let mut bytes = vec![0u8; 300 * 1024];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        std::fs::write(&path, &bytes).expect("write");
        assert_eq!(
            checksum_file(&path).expect("checksum").as_str(),
            hex_sha256(&bytes)
        );
    }

    #[test]
    fn checksum_file_reports_missing_file() {
        let dir = TempDir::new().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf())
            .expect("utf8")
            .join("absent");
        let err = checksum_file(&path).expect_err("missing");
        assert!(err.to_string().contains("failed to open"));
    }

    #[test]
    fn publish_moves_incomplete_and_creates_relative_alias() {
        let (_dir, env) = test_env();
        let name = FormulaName::from_str("wget").expect("name");
        let ver = PkgVersion::from_str("1.25.0").expect("ver");
        let file = bottle(EXAMPLE_URL);
        let paths = cache_paths(&env, &name, &file, &ver, 0).expect("paths");

        std::fs::create_dir_all(paths.final_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&paths.incomplete, b"bottle bytes").expect("write incomplete");

        publish(&paths).expect("publish");

        assert!(!paths.incomplete.exists(), "incomplete removed by rename");
        assert_eq!(
            std::fs::read(&paths.final_path).expect("final"),
            b"bottle bytes"
        );

        #[cfg(unix)]
        {
            let meta = std::fs::symlink_metadata(&paths.alias).expect("alias meta");
            assert!(meta.file_type().is_symlink(), "alias is a symlink");
            let target = std::fs::read_link(&paths.alias).expect("alias target");
            assert_eq!(target, std::path::Path::new(&paths.relative_target));
        }
    }

    #[test]
    fn ensure_alias_is_idempotent_and_replaces_stale_links() {
        let (_dir, env) = test_env();
        let name = FormulaName::from_str("wget").expect("name");
        let ver = PkgVersion::from_str("1.25.0").expect("ver");
        let file = bottle(EXAMPLE_URL);
        let paths = cache_paths(&env, &name, &file, &ver, 0).expect("paths");

        std::fs::create_dir_all(paths.final_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&paths.final_path, b"bytes").expect("write final");

        ensure_alias(&paths.alias, &paths.relative_target).expect("first ensure");
        ensure_alias(&paths.alias, &paths.relative_target).expect("idempotent second ensure");

        #[cfg(unix)]
        {
            // A stale symlink pointing elsewhere is replaced.
            std::fs::remove_file(&paths.alias).expect("remove");
            std::os::unix::fs::symlink("somewhere/else", &paths.alias).expect("stale link");
            ensure_alias(&paths.alias, &paths.relative_target).expect("replace stale");
            let target = std::fs::read_link(&paths.alias).expect("alias target");
            assert_eq!(target, std::path::Path::new(&paths.relative_target));

            // A plain file occupying the alias path is replaced (ln -sf semantics).
            std::fs::remove_file(&paths.alias).expect("remove");
            std::fs::write(&paths.alias, b"junk").expect("junk file");
            ensure_alias(&paths.alias, &paths.relative_target).expect("replace file");
            assert!(
                std::fs::symlink_metadata(&paths.alias)
                    .expect("meta")
                    .file_type()
                    .is_symlink()
            );
        }
    }
}
