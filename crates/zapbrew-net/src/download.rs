//! Bottle fetch into the Homebrew-compatible cache with resume and retries.

use std::collections::HashMap;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_RANGE, RANGE};
use reqwest::{Client, StatusCode, Url};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use zapbrew_prefix::Env;
use zapbrew_types::{BottleFile, Checksum, FormulaName, PkgVersion};

use crate::cache::{
    CachePaths, artifact_cache_paths_with_alias, bottle_basename, cache_paths, checksum_file,
    ensure_alias, publish,
};
use crate::error::NetError;
use crate::progress::download_progress;
use crate::types::{ArtifactDownloadRequest, CachedArtifact, CachedBottle, DownloadRequest};

/// Maximum download attempts (brew `retryable_download` default).
const MAX_ATTEMPTS: u32 = 5;

/// Default bottle root URL mirrored by the API; unset `HOMEBREW_BOTTLE_DOMAIN`
/// keeps the original `bottle.url` verbatim.
const BOTTLE_DEFAULT_DOMAIN: &str = "https://ghcr.io/v2/homebrew/core";

/// Internal policy for one artifact fetch.
///
/// `expected_checksum` gates publish; `force_download` skips cached final/incomplete
/// reuse so a `no_check` member can still force a fresh transfer without letting
/// unverified bytes become canonical.
#[derive(Debug, Clone)]
struct ArtifactFetchOptions {
    /// Declared digest, or `None` for a `no_check` artifact.
    expected_checksum: Option<Checksum>,
    /// Skip final/incomplete reuse and force a network transfer.
    force_download: bool,
}

/// Fetch one cask-style artifact into `$HOMEBREW_CACHE` with a fetch policy.
///
/// Uses the same content-addressed `downloads/` layout, `.incomplete` resume,
/// retry policy, and fsync + atomic publish as bottles, but sends no GHCR
/// bearer authorization header. Streams into `.incomplete`, verifies
/// `expected_checksum` there when present, then publishes; on mismatch only the
/// `.incomplete` file is removed and any previous final/alias is preserved.
async fn fetch_artifact_with_options(
    env: &Env,
    http: &Client,
    url: &str,
    alias_name: &str,
    options: &ArtifactFetchOptions,
) -> Result<CachedArtifact, NetError> {
    let paths = artifact_cache_paths_with_alias(env, url, alias_name)?;
    let expected = options.expected_checksum.as_ref();

    if !options.force_download {
        // With a declared checksum, a matching final file is immediately reusable.
        // Validate the cache path first and repair the alias before returning.
        if let Some(expected) = expected
            && cache_file_len(&env.cache, &paths.final_path)?.is_some()
            && let Ok(actual) = checksum_file(&paths.final_path)
            && &actual == expected
        {
            ensure_alias(&paths.alias, &paths.relative_target)?;
            return Ok(CachedArtifact {
                path: paths.final_path,
                alias: paths.alias,
                sha256: actual,
                reused: true,
            });
        }

        // With a declared checksum, a matching incomplete can be published without
        // any network I/O. A non-matching or invalid incomplete is left for the
        // retry loop so resume/restart and symlink validation work unchanged.
        if let Some(expected) = expected
            && cache_file_len(&env.cache, &paths.incomplete)?.is_some()
            && let Ok(actual) = checksum_file(&paths.incomplete)
            && &actual == expected
        {
            safe_publish(&env.cache, &paths)?;
            return Ok(CachedArtifact {
                path: paths.final_path.clone(),
                alias: paths.alias.clone(),
                sha256: actual,
                reused: false,
            });
        }
    }

    let mut last_error: Option<NetError> = None;

    for attempt in 0..MAX_ATTEMPTS {
        match attempt_artifact_download(env, http, url, &paths).await {
            Ok(()) => {
                if let Some(expected) = expected {
                    cache_file_len(&env.cache, &paths.incomplete)?.ok_or_else(|| {
                        unsafe_cache_path(
                            &paths.incomplete,
                            "incomplete path is not a regular file",
                        )
                    })?;
                    match checksum_file(&paths.incomplete) {
                        Ok(actual) if &actual == expected => {
                            safe_publish(&env.cache, &paths)?;
                            return Ok(CachedArtifact {
                                path: paths.final_path.clone(),
                                alias: paths.alias.clone(),
                                sha256: actual,
                                reused: false,
                            });
                        }
                        Ok(actual) => {
                            let _ = std::fs::remove_file(paths.incomplete.as_std_path());
                            last_error = Some(NetError::ChecksumMismatch {
                                expected: expected.clone(),
                                actual,
                                path: paths.incomplete.clone(),
                            });
                        }
                        Err(err) => {
                            let _ = std::fs::remove_file(paths.incomplete.as_std_path());
                            last_error = Some(err);
                        }
                    }
                } else {
                    cache_file_len(&env.cache, &paths.incomplete)?.ok_or_else(|| {
                        unsafe_cache_path(
                            &paths.incomplete,
                            "incomplete path is not a regular file",
                        )
                    })?;
                    let actual = checksum_file(&paths.incomplete)?;
                    safe_publish(&env.cache, &paths)?;
                    return Ok(CachedArtifact {
                        path: paths.final_path.clone(),
                        alias: paths.alias.clone(),
                        sha256: actual,
                        reused: false,
                    });
                }
            }
            Err(err) => {
                // Network / HTTP / IO / invalid-response: keep `.incomplete` for resume.
                last_error = Some(err);
            }
        }

        if attempt + 1 < MAX_ATTEMPTS {
            let secs = retry_delay_secs(attempt);
            if secs > 0 {
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| NetError::InvalidResponse {
        url: url.to_owned(),
        reason: "download failed without a recorded error".to_owned(),
    }))
}

/// Fetch one cask-style artifact into `$HOMEBREW_CACHE`.
///
/// Uses the same content-addressed `downloads/` layout, `.incomplete` resume,
/// retry policy, and fsync + atomic publish as bottles, but sends no GHCR
/// bearer authorization header. `request.sha256 == None` skips checksum
/// verification and keeps a conservative re-download policy for freshness.
pub async fn fetch_artifact(
    env: &Env,
    http: &Client,
    request: &ArtifactDownloadRequest,
) -> Result<CachedArtifact, NetError> {
    fetch_artifact_with_options(
        env,
        http,
        &request.url,
        &request.alias_name,
        &ArtifactFetchOptions {
            expected_checksum: request.sha256.clone(),
            force_download: false,
        },
    )
    .await
}

/// Fetch one bottle into `$HOMEBREW_CACHE`, reusing a valid final when present.
///
/// Streams into `<final>.incomplete`, resumes with `Range` only when the
/// server answers `206` with a matching `Content-Range` start, truncates and
/// restarts on a full `200`, retries up to five times, then publishes via the
/// cache module (`fsync` + atomic rename + alias).
pub async fn fetch_bottle(
    env: &Env,
    http: &Client,
    name: &FormulaName,
    bottle: &BottleFile,
    pkg_version: &PkgVersion,
    rebuild: u32,
) -> Result<CachedBottle, NetError> {
    let resolved_url = resolve_bottle_url(env, name, bottle, pkg_version, rebuild)?;
    let fallback_url = (resolved_url != bottle.url).then(|| bottle.url.clone());
    let mut effective_bottle = bottle.clone();
    effective_bottle.url = resolved_url.clone();
    let paths = cache_paths(env, name, &effective_bottle, pkg_version, rebuild)?;

    if paths.final_path.is_file()
        && let Ok(actual) = checksum_file(&paths.final_path)
        && actual == effective_bottle.sha256
    {
        return Ok(CachedBottle {
            path: paths.final_path,
            alias: paths.alias,
            reused: true,
        });
    }

    // Crash window: a fully written `.incomplete` that never reached `publish`
    // would otherwise Range past EOF (416) forever. Publish it if checksum matches.
    if let Some(cached) = try_publish_complete(&effective_bottle, &paths)? {
        return Ok(cached);
    }

    let mut last_error: Option<NetError> = None;
    for (origin, url) in [Some(resolved_url), fallback_url]
        .into_iter()
        .flatten()
        .enumerate()
    {
        if origin > 0 {
            let _ = std::fs::remove_file(paths.incomplete.as_std_path());
        }
        effective_bottle.url = url;
        for attempt in 0..MAX_ATTEMPTS {
            match attempt_download(env, http, &effective_bottle, &paths).await {
                Ok(()) => match checksum_file(&paths.incomplete) {
                    Ok(actual) if actual == effective_bottle.sha256 => {
                        publish(&paths)?;
                        return Ok(CachedBottle {
                            path: paths.final_path.clone(),
                            alias: paths.alias.clone(),
                            reused: false,
                        });
                    }
                    Ok(actual) => {
                        let _ = std::fs::remove_file(paths.incomplete.as_std_path());
                        last_error = Some(NetError::ChecksumMismatch {
                            expected: effective_bottle.sha256.clone(),
                            actual,
                            path: paths.incomplete.clone(),
                        });
                    }
                    Err(err) => {
                        let _ = std::fs::remove_file(paths.incomplete.as_std_path());
                        last_error = Some(err);
                    }
                },
                Err(err) => {
                    // Network / HTTP / IO / invalid-response: keep `.incomplete` for resume.
                    last_error = Some(err);
                }
            }

            if attempt + 1 < MAX_ATTEMPTS {
                let secs = retry_delay_secs(attempt);
                if secs > 0 {
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| NetError::InvalidResponse {
        url: effective_bottle.url,
        reason: "download failed without a recorded error".to_owned(),
    }))
}

/// Fetch many bottles with bounded concurrency, restoring input order.
///
/// Schedules at most `env.download_concurrency` futures via
/// `buffer_unordered`, then returns results in the same order as `items`.
/// The first error in input order is propagated.
pub async fn download_all(
    env: &Env,
    http: &Client,
    items: Vec<DownloadRequest>,
) -> Result<Vec<CachedBottle>, NetError> {
    let concurrency = env.download_concurrency.max(1);
    let count = items.len();
    let mut slots: Vec<Option<Result<CachedBottle, NetError>>> = Vec::with_capacity(count);
    slots.resize_with(count, || None);

    let mut unordered = stream::iter(items.into_iter().enumerate())
        .map(|(index, request)| async move {
            let result = fetch_bottle(
                env,
                http,
                &request.name,
                &request.bottle,
                &request.pkg_version,
                request.rebuild,
            )
            .await;
            (index, result)
        })
        .buffer_unordered(concurrency);

    while let Some((index, result)) = unordered.next().await {
        slots[index] = Some(result);
    }

    let mut bottles = Vec::with_capacity(count);
    for slot in slots {
        match slot {
            Some(Ok(bottle)) => bottles.push(bottle),
            Some(Err(err)) => return Err(err),
            None => {
                return Err(NetError::InvalidResponse {
                    url: String::new(),
                    reason: "download_all missing result slot".to_owned(),
                });
            }
        }
    }
    Ok(bottles)
}

/// Opaque validated and grouped batch of artifact downloads.
#[derive(Debug)]
pub struct PreparedArtifactDownloads {
    groups: Vec<PreparedGroup>,
    count: usize,
}

#[derive(Debug)]
struct PreparedGroup {
    url: String,
    members: Vec<PreparedMember>,
    /// First declared checksum for the group, used to detect conflicts and to
    /// verify the actual content after transfer.
    sha256: Option<Checksum>,
    /// True if any member is a `no_check` request. Any `no_check` forces a
    /// fresh transfer for the whole group; declared checksums are still
    /// verified against the actual content after download.
    has_no_check: bool,
    /// Canonical cache paths for the shared URL, validated before any network
    /// I/O and reused during fan-out.
    paths: CachePaths,
}

#[derive(Debug, Clone)]
struct PreparedMember {
    index: usize,
    alias_name: String,
    sha256: Option<Checksum>,
}

/// Validate every request, parse and scheme-check each URL, validate the cache
/// path for the URL basename and alias, run `cache_file_len` on the final and
/// incomplete paths to inspect static symlink/non-dir ancestors, group identical
/// URLs, store the canonical `CachePaths` per group, and reject conflicting
/// declared checksums or one alias mapped to distinct URLs before any network
/// I/O. The returned prepared batch is consumed by `download_artifacts_all`.
pub fn prepare_artifact_downloads(
    env: &Env,
    requests: Vec<ArtifactDownloadRequest>,
) -> Result<PreparedArtifactDownloads, NetError> {
    if requests.is_empty() {
        return Ok(PreparedArtifactDownloads {
            groups: Vec::new(),
            count: 0,
        });
    }

    let mut by_url: HashMap<String, PreparedGroup> = HashMap::new();
    let mut by_alias: HashMap<String, String> = HashMap::new();

    for (index, request) in requests.into_iter().enumerate() {
        let parsed = Url::parse(&request.url).map_err(|err| NetError::InvalidResponse {
            url: request.url.clone(),
            reason: format!("invalid URL: {err}"),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(NetError::InvalidResponse {
                url: request.url.clone(),
                reason: "URL must be absolute http or https".to_owned(),
            });
        }

        // Validate the cache path for this URL/alias and inspect final/incomplete.
        let paths = artifact_cache_paths_with_alias(env, &request.url, &request.alias_name)?;
        cache_file_len(&env.cache, &paths.final_path)?;
        cache_file_len(&env.cache, &paths.incomplete)?;
        match std::fs::symlink_metadata(&paths.alias) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                return Err(NetError::io(
                    "validate",
                    &paths.alias,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "alias path is a directory",
                    ),
                ));
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(NetError::io("inspect", &paths.alias, source)),
        }

        // Reject one alias_name mapped to distinct URLs.
        if let Some(existing_url) = by_alias.get(&request.alias_name) {
            if existing_url != &request.url {
                return Err(NetError::InvalidResponse {
                    url: request.url.clone(),
                    reason: format!("conflicting URLs for alias '{}'", request.alias_name),
                });
            }
        } else {
            by_alias.insert(request.alias_name.clone(), request.url.clone());
        }

        let group = by_url
            .entry(request.url.clone())
            .or_insert_with(|| PreparedGroup {
                url: request.url.clone(),
                members: Vec::new(),
                sha256: None,
                has_no_check: false,
                paths: paths.clone(),
            });

        if request.sha256.is_none() {
            group.has_no_check = true;
        }

        if let Some(sha256) = request.sha256.as_ref() {
            match &group.sha256 {
                None => group.sha256 = Some(sha256.clone()),
                Some(first) if first != sha256 => {
                    return Err(NetError::InvalidResponse {
                        url: request.url.clone(),
                        reason: "conflicting declared checksums for the same URL".to_owned(),
                    });
                }
                _ => {}
            }
        }

        group.members.push(PreparedMember {
            index,
            alias_name: request.alias_name,
            sha256: request.sha256,
        });
    }

    let count = by_url.values().map(|g| g.members.len()).sum();
    Ok(PreparedArtifactDownloads {
        groups: by_url.into_values().collect(),
        count,
    })
}

/// Fetch many generic artifacts with bounded concurrency, restoring input order.
///
/// Consumes a prepared batch from `prepare_artifact_downloads`. Groups identical
/// URLs so only one download is scheduled per content file, publishes the shared
/// content once, then fans out request-specific aliases. Returns results in the
/// same order as the input requests; the first error in input order is
/// propagated.
///
/// `request.sha256 == None` keeps the conservative `fetch_artifact` re-download
/// policy for `no_check` casks.
pub async fn download_artifacts_all(
    env: &Env,
    http: &Client,
    prepared: PreparedArtifactDownloads,
) -> Result<Vec<CachedArtifact>, NetError> {
    if prepared.groups.is_empty() {
        return Ok(Vec::new());
    }

    let concurrency = env.download_concurrency.max(1);
    let count = prepared.count;
    let mut slots: Vec<Option<Result<CachedArtifact, NetError>>> = Vec::with_capacity(count);
    slots.resize_with(count, || None);

    let mut unordered = stream::iter(prepared.groups)
        .map(|group| async move {
            // Any no_check in the group forces a fresh transfer. Use the first
            // no_check member as the representative so a mismatched declared
            // checksum does not create an alias for a Some request.
            let representative_index = if group.has_no_check {
                group
                    .members
                    .iter()
                    .position(|m| m.sha256.is_none())
                    .expect("has_no_check implies a no_check member")
            } else {
                0
            };
            let options = ArtifactFetchOptions {
                expected_checksum: group.sha256.clone(),
                force_download: group.has_no_check,
            };
            let result = fetch_artifact_with_options(
                env,
                http,
                &group.url,
                &group.members[representative_index].alias_name,
                &options,
            )
            .await;
            let representative_request_index = group.members[representative_index].index;
            (group, representative_request_index, result)
        })
        .buffer_unordered(concurrency);

    while let Some((group, representative_request_index, result)) = unordered.next().await {
        match result {
            Ok(artifact) => {
                for member in group.members {
                    if member.index == representative_request_index {
                        slots[member.index] = Some(Ok(artifact.clone()));
                    } else {
                        let alias_path = env.cache.join(member.alias_name.as_str());
                        ensure_alias(&alias_path, &group.paths.relative_target)?;
                        let mut result = artifact.clone();
                        result.alias = alias_path;
                        slots[member.index] = Some(Ok(result));
                    }
                }
            }
            Err(err) => {
                let mut members: Vec<_> = group.members.into_iter().collect();
                members.sort_by_key(|m| m.index);
                if let Some(first) = members.first() {
                    slots[first.index] = Some(Err(err));
                }
                for member in members.iter().skip(1) {
                    slots[member.index] = Some(Err(NetError::InvalidResponse {
                        url: group.url.clone(),
                        reason: "download failed for shared URL".to_owned(),
                    }));
                }
            }
        }
    }

    let mut artifacts = Vec::with_capacity(count);
    for slot in slots {
        match slot {
            Some(Ok(artifact)) => artifacts.push(artifact),
            Some(Err(err)) => return Err(err),
            None => {
                return Err(NetError::InvalidResponse {
                    url: String::new(),
                    reason: "download_artifacts_all missing result slot".to_owned(),
                });
            }
        }
    }
    Ok(artifacts)
}

/// One streaming cask-style artifact attempt (no checksum / publish).
async fn attempt_artifact_download(
    env: &Env,
    http: &Client,
    url: &str,
    paths: &CachePaths,
) -> Result<(), NetError> {
    let existing = cache_file_len(&env.cache, &paths.incomplete)?.unwrap_or(0);

    let mut request = http.get(url).header(ACCEPT, "application/octet-stream");

    if existing > 0 {
        request = request.header(RANGE, format!("bytes={existing}-"));
    }

    let response = request.send().await.map_err(|source| NetError::Http {
        url: url.to_owned(),
        source,
    })?;

    let status = response.status();
    let append = if status == StatusCode::PARTIAL_CONTENT {
        let start = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range_start);
        match start {
            Some(start) if start == existing => true,
            _ => {
                return Err(NetError::InvalidResponse {
                    url: url.to_owned(),
                    reason: format!("206 Content-Range missing or not starting at {existing}"),
                });
            }
        }
    } else if status == StatusCode::OK {
        false
    } else {
        return Err(NetError::InvalidResponse {
            url: url.to_owned(),
            reason: format!("unexpected status {status}"),
        });
    };

    if let Some(parent) = paths.incomplete.parent() {
        tokio::fs::create_dir_all(parent.as_std_path())
            .await
            .map_err(|source| NetError::io("create", parent, source))?;
    }
    let current = cache_file_len(&env.cache, &paths.incomplete)?.unwrap_or(0);
    if append && current != existing {
        return Err(unsafe_cache_path(
            &paths.incomplete,
            "incomplete download changed during resume",
        ));
    }

    let mut file = if append {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.incomplete.as_std_path())
            .await
            .map_err(|source| NetError::io("open", &paths.incomplete, source))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(paths.incomplete.as_std_path())
            .await
            .map_err(|source| NetError::io("open", &paths.incomplete, source))?
    };

    let content_len = response.content_length();
    let total = content_len.map(|len| {
        if append {
            existing.saturating_add(len)
        } else {
            len
        }
    });
    let progress = download_progress(env, total);
    if append {
        progress.set_position(existing);
    }

    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|source| NetError::Http {
            url: url.to_owned(),
            source,
        })?;
        file.write_all(&chunk)
            .await
            .map_err(|source| NetError::io("write", &paths.incomplete, source))?;
        progress.inc(chunk.len() as u64);
    }

    file.flush()
        .await
        .map_err(|source| NetError::io("flush", &paths.incomplete, source))?;
    progress.finish_and_clear();
    Ok(())
}

/// One streaming attempt into `paths.incomplete` (no checksum / publish).
async fn attempt_download(
    env: &Env,
    http: &Client,
    bottle: &BottleFile,
    paths: &CachePaths,
) -> Result<(), NetError> {
    let url = bottle.url.as_str();
    let existing = cache_file_len(&env.cache, &paths.incomplete)?.unwrap_or(0);

    let mut request = http.get(url).header(ACCEPT, "application/octet-stream");

    if is_ghcr_artifact_url(url)
        && let Some(auth) = ghcr_auth_header(env)
    {
        request = request.header(AUTHORIZATION, auth);
    }

    if existing > 0 {
        request = request.header(RANGE, format!("bytes={existing}-"));
    }

    let response = request.send().await.map_err(|source| NetError::Http {
        url: url.to_owned(),
        source,
    })?;

    let status = response.status();
    let append = if status == StatusCode::PARTIAL_CONTENT {
        let start = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range_start);
        match start {
            Some(start) if start == existing => true,
            _ => {
                return Err(NetError::InvalidResponse {
                    url: url.to_owned(),
                    reason: format!("206 Content-Range missing or not starting at {existing}"),
                });
            }
        }
    } else if status == StatusCode::OK {
        false
    } else {
        return Err(NetError::InvalidResponse {
            url: url.to_owned(),
            reason: format!("unexpected status {status}"),
        });
    };

    if let Some(parent) = paths.incomplete.parent() {
        tokio::fs::create_dir_all(parent.as_std_path())
            .await
            .map_err(|source| NetError::io("create", parent, source))?;
    }
    let current = cache_file_len(&env.cache, &paths.incomplete)?.unwrap_or(0);
    if append && current != existing {
        return Err(unsafe_cache_path(
            &paths.incomplete,
            "incomplete download changed during resume",
        ));
    }

    let mut file = if append {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.incomplete.as_std_path())
            .await
            .map_err(|source| NetError::io("open", &paths.incomplete, source))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(paths.incomplete.as_std_path())
            .await
            .map_err(|source| NetError::io("open", &paths.incomplete, source))?
    };

    let content_len = response.content_length();
    let total = content_len.map(|len| {
        if append {
            existing.saturating_add(len)
        } else {
            len
        }
    });
    let progress = download_progress(env, total);
    if append {
        progress.set_position(existing);
    }

    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|source| NetError::Http {
            url: url.to_owned(),
            source,
        })?;
        file.write_all(&chunk)
            .await
            .map_err(|source| NetError::io("write", &paths.incomplete, source))?;
        progress.inc(chunk.len() as u64);
    }

    file.flush()
        .await
        .map_err(|source| NetError::io("flush", &paths.incomplete, source))?;
    progress.finish_and_clear();
    Ok(())
}

/// If `paths.incomplete` already holds the expected digest, publish and return it.
fn try_publish_complete(
    bottle: &BottleFile,
    paths: &CachePaths,
) -> Result<Option<CachedBottle>, NetError> {
    if !paths.incomplete.is_file() {
        return Ok(None);
    }

    match checksum_file(&paths.incomplete) {
        Ok(actual) if actual == bottle.sha256 => {
            publish(paths)?;
            Ok(Some(CachedBottle {
                path: paths.final_path.clone(),
                alias: paths.alias.clone(),
                reused: false,
            }))
        }
        Ok(_) | Err(_) => Ok(None),
    }
}

/// Internal retry delay policy: production uses `2^attempt` seconds; test builds
/// of this crate are zero-delay. Integration tests (`tests/`) compile the library
/// without `cfg(test)` and use `start_paused = true` so production sleeps auto-advance.
fn retry_delay_secs(attempt: u32) -> u64 {
    RetryDelay::current().secs(attempt)
}

fn ghcr_auth_header(env: &Env) -> Option<String> {
    if let Some(token) = env.docker_registry_token.as_deref() {
        return Some(format!("Bearer {token}"));
    }

    if let Some(token) = env.docker_registry_basic_auth_token.as_deref() {
        if token == "none" {
            return None;
        }
        return Some(format!("Basic {token}"));
    }

    Some("Bearer QQ==".to_owned())
}

fn is_ghcr_artifact_url(url: &str) -> bool {
    let Some(path) = url.strip_prefix("https://ghcr.io/v2/") else {
        return false;
    };
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .count()
        >= 3
}

fn resolve_bottle_url(
    env: &Env,
    name: &FormulaName,
    bottle: &BottleFile,
    pkg_version: &PkgVersion,
    rebuild: u32,
) -> Result<String, NetError> {
    if env.bottle_domain == BOTTLE_DEFAULT_DOMAIN {
        return Ok(bottle.url.clone());
    }

    let root = normalize_bottle_domain(&env.bottle_domain);
    if is_ghcr_root(&root) {
        let image_name = github_image_name(name.name());
        Ok(format!(
            "{root}/{image_name}/blobs/sha256:{}",
            bottle.sha256
        ))
    } else {
        let filename = bottle_basename(name, pkg_version, bottle.tag, rebuild)?;
        let encoded = percent_encode_path_segment(&filename);
        Ok(format!("{root}/{encoded}"))
    }
}

fn normalize_bottle_domain(domain: &str) -> String {
    let domain = domain.trim_end_matches('/');
    let Some(rest) = domain.strip_prefix("docker://ghcr.io/") else {
        return domain.to_owned();
    };
    let mut segments = rest.split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some(org), Some(repo), None) if !org.is_empty() && !repo.is_empty() => {
            format!("https://ghcr.io/v2/{org}/{repo}")
        }
        _ => domain.to_owned(),
    }
}

/// Exact HTTPS GHCR OCI roots only.
fn is_ghcr_root(root: &str) -> bool {
    let root = root.trim_end_matches('/');
    let Some(path) = root.strip_prefix("https://ghcr.io/v2/") else {
        return false;
    };
    if root.contains('?') || root.contains('#') {
        return false;
    }
    let mut segments = path.split('/');
    matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some(org), Some(repo), None) if !org.is_empty() && !repo.is_empty()
    )
}

fn github_image_name(name: &str) -> String {
    name.replace('@', "/").replace('+', "x")
}

fn percent_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

#[derive(Clone, Copy)]
enum RetryDelay {
    Production,
    Zero,
}

impl RetryDelay {
    fn current() -> Self {
        if cfg!(test) {
            Self::Zero
        } else {
            Self::Production
        }
    }

    fn secs(self, attempt: u32) -> u64 {
        match self {
            Self::Zero => 0,
            Self::Production => 1u64 << attempt,
        }
    }
}

fn cache_file_len(
    cache: &camino::Utf8Path,
    path: &camino::Utf8Path,
) -> Result<Option<u64>, NetError> {
    if path == cache || !path.starts_with(cache) {
        return Err(unsafe_cache_path(path, "path is outside the cache"));
    }

    let mut current = path;
    let mut len: Option<u64> = None;
    loop {
        match std::fs::symlink_metadata(current.as_std_path()) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(unsafe_cache_path(current, "path is a symlink"));
            }
            Ok(meta) if current == path && !meta.is_file() => {
                return Err(unsafe_cache_path(path, "cache path is not a regular file"));
            }
            Ok(meta) if current != path && !meta.is_dir() => {
                return Err(unsafe_cache_path(
                    current,
                    "cache ancestor is not a directory",
                ));
            }
            Ok(meta) if current == path => len = Some(meta.len()),
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(NetError::io("inspect", current, source)),
        }
        if current == cache {
            break;
        }
        current = current
            .parent()
            .ok_or_else(|| unsafe_cache_path(path, "path has no cache-root ancestor"))?;
    }
    Ok(len)
}

fn safe_publish(cache: &camino::Utf8Path, paths: &CachePaths) -> Result<(), NetError> {
    cache_file_len(cache, &paths.incomplete)?.ok_or_else(|| {
        unsafe_cache_path(&paths.incomplete, "incomplete path is not a regular file")
    })?;
    cache_file_len(cache, &paths.final_path)?;
    publish(paths)
}

fn unsafe_cache_path(path: &camino::Utf8Path, reason: &'static str) -> NetError {
    NetError::io(
        "validate",
        path,
        std::io::Error::new(std::io::ErrorKind::InvalidInput, reason),
    )
}

/// Parse the start offset from a `Content-Range` value like `bytes 5-22/23`.
fn parse_content_range_start(value: &str) -> Option<u64> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (start, _) = rest.split_once('-')?;
    start.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::str::FromStr;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};
    use zapbrew_types::{BottleFile, BottleTag, Checksum, FormulaName, PkgVersion};

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

    fn bottle(url: &str, sha: &str) -> BottleFile {
        BottleFile {
            tag: BottleTag::from_str("x86_64_linux").expect("tag"),
            cellar: "any".into(),
            url: url.to_owned(),
            sha256: Checksum::from_str(sha).expect("sha"),
        }
    }

    fn name() -> FormulaName {
        FormulaName::from_str("wget").expect("name")
    }

    fn version() -> PkgVersion {
        PkgVersion::from_str("1.25.0").expect("version")
    }

    fn env_with_bottle_domain(bottle_domain: &str) -> (TempDir, Env) {
        let (dir, mut env) = test_env();
        env.bottle_domain = bottle_domain.to_owned();
        (dir, env)
    }

    #[test]
    fn default_bottle_domain_preserves_original_url() {
        let (_dir, env) = env_with_bottle_domain(BOTTLE_DEFAULT_DOMAIN);
        let bottle = bottle("https://example.test/original", "a".repeat(64).as_str());
        let resolved = resolve_bottle_url(&env, &name(), &bottle, &version(), 0).expect("resolve");
        assert_eq!(resolved, "https://example.test/original");
    }

    #[test]
    fn https_ghcr_root_rewrites_to_oci_blob_url() {
        let (_dir, env) = env_with_bottle_domain("https://ghcr.io/v2/foo/bar");
        let sha = "a".repeat(64);
        let bottle = bottle("https://ignored.test/", &sha);
        let resolved = resolve_bottle_url(&env, &name(), &bottle, &version(), 0).expect("resolve");
        assert_eq!(
            resolved,
            format!("https://ghcr.io/v2/foo/bar/wget/blobs/sha256:{sha}")
        );
    }

    #[test]
    fn docker_ghcr_scheme_normalizes_to_https_v2() {
        let (_dir, env) = env_with_bottle_domain("docker://ghcr.io/foo/bar");
        let sha = "a".repeat(64);
        let bottle = bottle("https://ignored.test/", &sha);
        let resolved = resolve_bottle_url(&env, &name(), &bottle, &version(), 0).expect("resolve");
        assert_eq!(
            resolved,
            format!("https://ghcr.io/v2/foo/bar/wget/blobs/sha256:{sha}")
        );
    }

    #[test]
    fn http_ghcr_root_is_flat_mirror_not_oci() {
        let (_dir, env) = env_with_bottle_domain("http://ghcr.io/v2/foo/bar");
        let sha = "a".repeat(64);
        let bottle = bottle("https://ignored.test/", &sha);
        let resolved = resolve_bottle_url(&env, &name(), &bottle, &version(), 0).expect("resolve");
        assert_eq!(
            resolved,
            "http://ghcr.io/v2/foo/bar/wget--1.25.0.x86_64_linux.bottle.tar.gz"
        );
    }

    #[test]
    fn custom_v2_mirror_is_flat_not_oci() {
        let (_dir, env) = env_with_bottle_domain("https://mirror.example/v2/foo/bar");
        let sha = "a".repeat(64);
        let bottle = bottle("https://ignored.test/", &sha);
        let resolved = resolve_bottle_url(&env, &name(), &bottle, &version(), 0).expect("resolve");
        assert_eq!(
            resolved,
            "https://mirror.example/v2/foo/bar/wget--1.25.0.x86_64_linux.bottle.tar.gz"
        );
    }

    #[test]
    fn is_ghcr_root_rejects_non_ghcr_and_non_https() {
        assert!(is_ghcr_root("https://ghcr.io/v2/homebrew/core"));
        assert!(is_ghcr_root("https://ghcr.io/v2/foo/bar"));
        assert!(!is_ghcr_root("https://ghcr.io/v2/foo/bar/baz"));

        assert!(!is_ghcr_root("http://ghcr.io/v2/homebrew/core"));
        assert!(!is_ghcr_root("https://mirror.example/v2/homebrew/core"));
        assert!(!is_ghcr_root("https://ghcr.io/v2/homebrew"));
        assert!(!is_ghcr_root("https://ghcr.io/v2/"));
        assert!(!is_ghcr_root("https://ghcr.io/v2/homebrew//core"));
        assert!(!is_ghcr_root("https://ghcr.io/v2/homebrew/core?ref=main"));
        assert!(!is_ghcr_root(
            "https://evil.com/https://ghcr.io/v2/homebrew/core"
        ));
    }

    fn auth_env(docker: Option<&str>, basic: Option<&str>) -> (TempDir, Env) {
        let (dir, mut env) = test_env();
        env.docker_registry_token = docker.map(|s| s.to_owned());
        env.docker_registry_basic_auth_token = basic.map(|s| s.to_owned());
        (dir, env)
    }

    #[test]
    fn auth_default_is_bearer_qq() {
        let (_dir, env) = auth_env(None, None);
        assert_eq!(ghcr_auth_header(&env), Some("Bearer QQ==".to_owned()));
    }

    #[test]
    fn auth_docker_token_precedes_basic() {
        let (_dir, env) = auth_env(Some("docker-t"), Some("basic-t"));
        assert_eq!(ghcr_auth_header(&env), Some("Bearer docker-t".to_owned()));
    }

    #[test]
    fn auth_basic_token_used_when_no_docker() {
        let (_dir, env) = auth_env(None, Some("basic-t"));
        assert_eq!(ghcr_auth_header(&env), Some("Basic basic-t".to_owned()));
    }

    #[test]
    fn auth_basic_none_omits_header() {
        let (_dir, env) = auth_env(None, Some("none"));
        assert_eq!(ghcr_auth_header(&env), None);
    }

    #[test]
    fn auth_docker_none_is_a_bearer_token() {
        let (_dir, env) = auth_env(Some("none"), Some("basic-t"));
        assert_eq!(ghcr_auth_header(&env), Some("Bearer none".to_owned()));
    }
}
