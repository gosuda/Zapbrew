//! Bottle fetch into the Homebrew-compatible cache with resume and retries.

use std::time::Duration;

use futures::stream::{self, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_RANGE, RANGE};
use reqwest::{Client, StatusCode};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use zapbrew_prefix::Env;
use zapbrew_types::{BottleFile, Checksum, FormulaName, PkgVersion};

use crate::cache::{CachePaths, artifact_cache_paths, cache_paths, checksum_file, publish};
use crate::error::NetError;
use crate::progress::download_progress;
use crate::types::{CachedArtifact, CachedBottle, DownloadRequest};

/// Maximum download attempts (brew `retryable_download` default).
const MAX_ATTEMPTS: u32 = 5;

/// Fetch one cask-style artifact into `$HOMEBREW_CACHE`.
///
/// Uses the same content-addressed `downloads/` layout, `.incomplete` resume,
/// retry policy, and fsync + atomic publish as bottles, but sends no GHCR
/// bearer authorization header. `sha256 == None` skips checksum verification.
pub async fn fetch_artifact(
    env: &Env,
    http: &Client,
    url: &str,
    sha256: Option<&Checksum>,
) -> Result<CachedArtifact, NetError> {
    let paths = artifact_cache_paths(env, url)?;
    let expected = sha256;

    if let Some(expected) = expected
        && paths.final_path.is_file()
        && let Ok(actual) = checksum_file(&paths.final_path)
        && &actual == expected
    {
        return Ok(CachedArtifact {
            path: paths.final_path,
            alias: paths.alias,
            reused: true,
        });
    }

    if let Some(expected) = expected
        && paths.incomplete.is_file()
        && let Ok(actual) = checksum_file(&paths.incomplete)
        && &actual == expected
    {
        publish(&paths)?;
        return Ok(CachedArtifact {
            path: paths.final_path,
            alias: paths.alias,
            reused: false,
        });
    }

    let mut last_error: Option<NetError> = None;

    for attempt in 0..MAX_ATTEMPTS {
        match attempt_artifact_download(env, http, url, &paths).await {
            Ok(()) => {
                if let Some(expected) = expected {
                    match checksum_file(&paths.incomplete) {
                        Ok(actual) if &actual == expected => {
                            publish(&paths)?;
                            return Ok(CachedArtifact {
                                path: paths.final_path.clone(),
                                alias: paths.alias.clone(),
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
                    publish(&paths)?;
                    return Ok(CachedArtifact {
                        path: paths.final_path.clone(),
                        alias: paths.alias.clone(),
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
    let paths = cache_paths(env, name, bottle, pkg_version, rebuild)?;

    if paths.final_path.is_file()
        && let Ok(actual) = checksum_file(&paths.final_path)
        && actual == bottle.sha256
    {
        return Ok(CachedBottle {
            path: paths.final_path,
            alias: paths.alias,
            reused: true,
        });
    }

    // Crash window: a fully written `.incomplete` that never reached `publish`
    // would otherwise Range past EOF (416) forever. Publish it if checksum matches.
    if let Some(cached) = try_publish_complete(bottle, &paths)? {
        return Ok(cached);
    }

    let mut last_error: Option<NetError> = None;

    for attempt in 0..MAX_ATTEMPTS {
        match attempt_download(env, http, bottle, &paths).await {
            Ok(()) => match checksum_file(&paths.incomplete) {
                Ok(actual) if actual == bottle.sha256 => {
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
                        expected: bottle.sha256.clone(),
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

    Err(last_error.unwrap_or_else(|| NetError::InvalidResponse {
        url: bottle.url.clone(),
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

/// One streaming cask-style artifact attempt (no checksum / publish).
async fn attempt_artifact_download(
    env: &Env,
    http: &Client,
    url: &str,
    paths: &CachePaths,
) -> Result<(), NetError> {
    let existing = incomplete_len(&paths.incomplete)?;

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
    let existing = incomplete_len(&paths.incomplete)?;

    let mut request = http
        .get(url)
        .header(AUTHORIZATION, format!("Bearer {}", bearer_token(env)))
        .header(ACCEPT, "application/octet-stream");

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

fn bearer_token(env: &Env) -> &str {
    env.docker_registry_token
        .as_deref()
        .or(env.github_packages_token.as_deref())
        .unwrap_or("QQ==")
}

/// Internal retry delay policy: production uses `2^attempt` seconds; test builds
/// of this crate are zero-delay. Integration tests (`tests/`) compile the library
/// without `cfg(test)` and use `start_paused = true` so production sleeps auto-advance.
fn retry_delay_secs(attempt: u32) -> u64 {
    RetryDelay::current().secs(attempt)
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

fn incomplete_len(path: &camino::Utf8Path) -> Result<u64, NetError> {
    match std::fs::metadata(path.as_std_path()) {
        Ok(meta) => Ok(meta.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(NetError::io("stat", path, err)),
    }
}

/// Parse the start offset from a `Content-Range` value like `bytes 5-22/23`.
fn parse_content_range_start(value: &str) -> Option<u64> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (start, _) = rest.split_once('-')?;
    start.trim().parse().ok()
}
