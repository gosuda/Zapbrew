//! Fetch, verify, and cache signed Homebrew JSON API envelopes.
//!
//! Cache layout: `{env.cache}/api/{file}`. Writes go through
//! `{path}.incomplete.{pid}.{seq}` + `sync_all` + `rename` so an unverified body never
//! replaces a previously verified envelope.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use camino::{Utf8Path, Utf8PathBuf};
use jiff::Timestamp;
use jiff::fmt::rfc2822::DateTimePrinter;
use reqwest::StatusCode;
use zapbrew_prefix::Env;

use crate::error::ApiError;
use crate::jws::JwsVerifier;

/// Default Homebrew JSON API domain (error URLs and retry target).
const DEFAULT_API_DOMAIN: &str = "https://formulae.brew.sh/api";

/// Typed load warning recorded by the API crate (no output dependency).
///
/// [`Display`] yields the exact brew body without a `Warning: ` prefix, e.g.
/// `formula.jws.json: update failed, falling back to cached version.`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiWarning {
    /// Network refresh failed; a verified on-disk cache was returned instead.
    CacheFallback {
        /// Endpoint path under the API domain (usually a basename like
        /// `formula.jws.json`).
        file: String,
    },
}

impl ApiWarning {
    /// Exact warning body (same text as [`Display`]).
    #[must_use]
    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for ApiWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CacheFallback { file } => write!(
                f,
                "{}: update failed, falling back to cached version.",
                endpoint_basename(file)
            ),
        }
    }
}

/// Cache freshness policy for [`load_signed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheMode {
    /// Honor TTL / `HOMEBREW_NO_AUTO_UPDATE`.
    Normal,
    /// Ignore TTL and `no_auto_update`; always attempt a network refresh.
    Force,
}

/// Verified payload returned by [`load_signed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadedPayload {
    /// Verified inner JWS payload string (unchanged from the envelope).
    pub payload: String,
    /// Prior verified payload when [`CacheMode::Force`] replaced the cache.
    pub previous: Option<String>,
    /// Present when a network refresh failed and a verified cache was used.
    pub warning: Option<ApiWarning>,
}

/// Load a signed API file: fetch if needed, verify via `verifier`, cache under
/// `{env.cache}/api/{file}`, and return the verified payload string.
pub(crate) async fn load_signed(
    env: &Env,
    http: &reqwest::Client,
    file: &str,
    mode: CacheMode,
    verifier: &dyn JwsVerifier,
) -> Result<LoadedPayload, ApiError> {
    load_signed_with(env, http, file, mode, verifier, DEFAULT_API_DOMAIN).await
}

async fn load_signed_with(
    env: &Env,
    http: &reqwest::Client,
    file: &str,
    mode: CacheMode,
    verifier: &dyn JwsVerifier,
    default_api_domain: &str,
) -> Result<LoadedPayload, ApiError> {
    let cache = cache_path(env, file);
    let default_url = join_url(default_api_domain, file);

    // Force: verify any existing cache up front so a later failed download can
    // fall back without re-reading, and so corrupt caches are cleared early.
    let preserved = match mode {
        CacheMode::Force => match read_verified_cache(&cache, verifier) {
            Ok(Some(payload)) => Some(payload),
            Ok(None) => None,
            Err(_) => {
                let _ = fs::remove_file(&cache);
                None
            }
        },
        CacheMode::Normal => None,
    };

    if mode == CacheMode::Normal && cache_usable(env, &cache) {
        match read_verified_cache(&cache, verifier) {
            Ok(Some(payload)) => {
                return Ok(LoadedPayload {
                    payload,
                    previous: None,
                    warning: None,
                });
            }
            Ok(None) => {}
            Err(_) => {
                let _ = fs::remove_file(&cache);
                return fetch_and_store(
                    env,
                    http,
                    file,
                    &cache,
                    mode,
                    None,
                    verifier,
                    default_api_domain,
                    &default_url,
                    true,
                )
                .await;
            }
        }
    }

    fetch_and_store(
        env,
        http,
        file,
        &cache,
        mode,
        preserved,
        verifier,
        default_api_domain,
        &default_url,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn fetch_and_store(
    env: &Env,
    http: &reqwest::Client,
    file: &str,
    cache: &Utf8Path,
    mode: CacheMode,
    mut preserved: Option<String>,
    verifier: &dyn JwsVerifier,
    default_api_domain: &str,
    default_url: &str,
    mut after_corrupt_cache: bool,
) -> Result<LoadedPayload, ApiError> {
    loop {
        match download_envelope(env, http, file, cache, mode, default_api_domain).await {
            Ok(DownloadOutcome::NotModified) => {
                touch_mtime(cache)?;
                match read_verified_cache(cache, verifier) {
                    Ok(Some(payload)) => {
                        return Ok(LoadedPayload {
                            payload,
                            previous: force_previous(mode, preserved.as_ref()),
                            warning: None,
                        });
                    }
                    Ok(None) | Err(_) => {
                        let _ = fs::remove_file(cache);
                        if after_corrupt_cache {
                            return Err(ApiError::CannotDownload {
                                url: default_url.to_owned(),
                            });
                        }
                        preserved = None;
                        after_corrupt_cache = true;
                        continue;
                    }
                }
            }
            Ok(DownloadOutcome::Body(bytes)) => match verifier.verify(&bytes) {
                Ok(payload_bytes) => {
                    let payload = String::from_utf8(payload_bytes).map_err(|err| {
                        ApiError::invalid(file, format!("JWS payload is not UTF-8: {err}"))
                    })?;
                    write_atomic(cache, &bytes)?;
                    return Ok(LoadedPayload {
                        payload,
                        previous: force_previous(mode, preserved.as_ref()),
                        warning: None,
                    });
                }
                Err(_) => {
                    let _ = fs::remove_file(incomplete_path(cache));
                    // Keep a still-verified on-disk envelope until a verified
                    // replacement is renamed into place.
                    if after_corrupt_cache {
                        if read_verified_cache(cache, verifier)
                            .ok()
                            .flatten()
                            .is_none()
                        {
                            let _ = fs::remove_file(cache);
                        }
                        return Err(ApiError::CannotDownload {
                            url: default_url.to_owned(),
                        });
                    }
                    after_corrupt_cache = true;
                    continue;
                }
            },
            Err(_) => {
                return fallback_to_cache(verifier, file, cache, preserved, default_url);
            }
        }
    }
}

fn force_previous(mode: CacheMode, preserved: Option<&String>) -> Option<String> {
    match mode {
        CacheMode::Force => preserved.cloned(),
        CacheMode::Normal => None,
    }
}

fn fallback_to_cache(
    verifier: &dyn JwsVerifier,
    file: &str,
    cache: &Utf8Path,
    preserved: Option<String>,
    default_url: &str,
) -> Result<LoadedPayload, ApiError> {
    if let Some(payload) = preserved {
        return Ok(LoadedPayload {
            payload,
            previous: None,
            warning: Some(ApiWarning::CacheFallback {
                file: file.to_owned(),
            }),
        });
    }

    match read_verified_cache(cache, verifier) {
        Ok(Some(payload)) => Ok(LoadedPayload {
            payload,
            previous: None,
            warning: Some(ApiWarning::CacheFallback {
                file: file.to_owned(),
            }),
        }),
        Ok(None) | Err(_) => {
            let _ = fs::remove_file(cache);
            Err(ApiError::CannotDownload {
                url: default_url.to_owned(),
            })
        }
    }
}

enum DownloadOutcome {
    NotModified,
    Body(Vec<u8>),
}

async fn download_envelope(
    env: &Env,
    http: &reqwest::Client,
    file: &str,
    cache: &Utf8Path,
    mode: CacheMode,
    default_api_domain: &str,
) -> Result<DownloadOutcome, ApiError> {
    let primary = join_url(&env.api_domain, file);
    let conditional = mode == CacheMode::Normal && cache_exists_nonempty(cache);

    match try_get(http, &primary, cache, conditional).await {
        Ok(outcome) => Ok(outcome),
        Err(first_err) => {
            let default_url = join_url(default_api_domain, file);
            if urls_equivalent(&primary, &default_url) {
                return Err(first_err);
            }
            // Retry once against the default domain (unconditional GET).
            try_get(http, &default_url, cache, false)
                .await
                .map_err(|_| first_err)
        }
    }
}

async fn try_get(
    http: &reqwest::Client,
    url: &str,
    cache: &Utf8Path,
    conditional: bool,
) -> Result<DownloadOutcome, ApiError> {
    let mut req = http.get(url);
    if conditional && let Some(ims) = if_modified_since_header(cache)? {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, ims);
    }

    let response = req.send().await.map_err(|source| ApiError::Http {
        url: url.to_owned(),
        source,
    })?;

    let status = response.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(DownloadOutcome::NotModified);
    }
    if !status.is_success() {
        return Err(ApiError::invalid(
            url,
            format!("unexpected HTTP status {status}"),
        ));
    }

    let bytes = response.bytes().await.map_err(|source| ApiError::Http {
        url: url.to_owned(),
        source,
    })?;
    Ok(DownloadOutcome::Body(bytes.to_vec()))
}

fn cache_usable(env: &Env, cache: &Utf8Path) -> bool {
    if !cache_exists_nonempty(cache) {
        return false;
    }
    if env.no_auto_update {
        return true;
    }
    is_fresh(cache, env.api_auto_update_secs)
}

fn is_fresh(path: &Utf8Path, ttl_secs: u64) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    match SystemTime::now().duration_since(mtime) {
        Ok(age) => age < Duration::from_secs(ttl_secs),
        // Clock skew / future mtime: treat as fresh.
        Err(_) => true,
    }
}

fn cache_exists_nonempty(path: &Utf8Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

fn read_verified_cache(
    path: &Utf8Path,
    verifier: &dyn JwsVerifier,
) -> Result<Option<String>, ApiError> {
    if !cache_exists_nonempty(path) {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|source| ApiError::io("read", path, source))?;
    let payload = verifier.verify(&bytes)?;
    let payload = String::from_utf8(payload).map_err(|err| {
        ApiError::invalid(path.as_str(), format!("JWS payload is not UTF-8: {err}"))
    })?;
    Ok(Some(payload))
}

fn write_atomic(path: &Utf8Path, bytes: &[u8]) -> Result<(), ApiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            ApiError::io("create directory", Utf8PathBuf::from(parent), source)
        })?;
    }
    // Unique incomplete name so concurrent load_signed calls cannot rename
    // each other's temp files.
    static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let incomplete = Utf8PathBuf::from(format!("{path}.incomplete.{}.{}", std::process::id(), seq));
    {
        let mut file = File::create(&incomplete)
            .map_err(|source| ApiError::io("create", &incomplete, source))?;
        file.write_all(bytes)
            .map_err(|source| ApiError::io("write", &incomplete, source))?;
        file.sync_all()
            .map_err(|source| ApiError::io("sync", &incomplete, source))?;
    }
    fs::rename(&incomplete, path).map_err(|source| {
        let _ = fs::remove_file(&incomplete);
        ApiError::io("rename", path, source)
    })?;
    Ok(())
}

fn incomplete_path(path: &Utf8Path) -> Utf8PathBuf {
    // Legacy single-name incomplete path (best-effort cleanup / test probe).
    Utf8PathBuf::from(format!("{path}.incomplete"))
}

fn touch_mtime(path: &Utf8Path) -> Result<(), ApiError> {
    let file = File::options()
        .write(true)
        .open(path)
        .map_err(|source| ApiError::io("open", path, source))?;
    file.set_modified(SystemTime::now())
        .map_err(|source| ApiError::io("set modified time", path, source))?;
    Ok(())
}

fn if_modified_since_header(path: &Utf8Path) -> Result<Option<String>, ApiError> {
    let meta = fs::metadata(path).map_err(|source| ApiError::io("stat", path, source))?;
    let mtime = meta
        .modified()
        .map_err(|source| ApiError::io("read modified time", path, source))?;
    let ts = Timestamp::try_from(mtime).map_err(|err| {
        ApiError::invalid(
            path.as_str(),
            format!("mtime outside Timestamp range: {err}"),
        )
    })?;
    let header = DateTimePrinter::new()
        .timestamp_to_rfc9110_string(&ts)
        .map_err(|err| {
            ApiError::invalid(path.as_str(), format!("format If-Modified-Since: {err}"))
        })?;
    Ok(Some(header))
}

fn cache_path(env: &Env, file: &str) -> Utf8PathBuf {
    let mut path = env.cache.join("api");
    for component in file.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            continue;
        }
        path.push(component);
    }
    path
}

fn join_url(domain: &str, file: &str) -> String {
    let domain = domain.trim_end_matches('/');
    let file = file.trim_start_matches('/');
    format!("{domain}/{file}")
}

fn urls_equivalent(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

fn endpoint_basename(endpoint: &str) -> &str {
    Path::new(endpoint)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use tempfile::TempDir;
    use wiremock::matchers::{header_exists, method, path as url_path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};
    use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, EnvDetectInput};

    struct PanicRunner;

    impl CommandRunner for PanicRunner {
        fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
            panic!("Linux Env::detect_from must not spawn processes");
        }
    }

    /// Envelope → payload extractor that can reject selected envelopes.
    struct FakeVerifier {
        reject_once: Mutex<Vec<Vec<u8>>>,
        reject_always: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeVerifier {
        fn new() -> Self {
            Self {
                reject_once: Mutex::new(Vec::new()),
                reject_always: Mutex::new(Vec::new()),
            }
        }

        fn reject_once_envelope(&self, envelope: &[u8]) {
            self.reject_once
                .lock()
                .expect("reject_once lock")
                .push(envelope.to_vec());
        }

        fn reject_always_envelope(&self, envelope: &[u8]) {
            self.reject_always
                .lock()
                .expect("reject_always lock")
                .push(envelope.to_vec());
        }
    }

    impl JwsVerifier for FakeVerifier {
        fn verify(&self, envelope: &[u8]) -> Result<Vec<u8>, ApiError> {
            {
                let mut once = self.reject_once.lock().expect("reject_once lock");
                if let Some(idx) = once.iter().position(|e| e == envelope) {
                    once.remove(idx);
                    return Err(ApiError::Signature {
                        reason: "fake reject once".into(),
                    });
                }
            }
            if self
                .reject_always
                .lock()
                .expect("reject_always lock")
                .iter()
                .any(|e| e == envelope)
            {
                return Err(ApiError::Signature {
                    reason: "fake reject".into(),
                });
            }

            let value: serde_json::Value =
                serde_json::from_slice(envelope).map_err(|source| ApiError::Json {
                    context: "JWS envelope".into(),
                    source,
                })?;
            let payload = value
                .get("payload")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::invalid("JWS envelope", "missing string payload"))?;
            Ok(payload.as_bytes().to_vec())
        }
    }

    fn envelope(payload: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "payload": payload,
            "signatures": [{"protected": "e30", "header": {"kid": "test"}, "signature": "AA"}]
        }))
        .expect("envelope json")
    }

    fn test_env(cache: &Utf8Path, api_domain: &str, ttl_secs: u64, no_auto_update: bool) -> Env {
        let home = cache
            .parent()
            .map(Utf8Path::to_path_buf)
            .unwrap_or_else(|| cache.to_path_buf());
        let prefix = home.join("prefix");
        let mut vars = HashMap::new();
        vars.insert("HOMEBREW_PREFIX".into(), prefix.to_string());
        vars.insert("HOMEBREW_CACHE".into(), cache.to_string());
        vars.insert("HOMEBREW_API_DOMAIN".into(), api_domain.to_owned());
        vars.insert("HOMEBREW_API_AUTO_UPDATE_SECS".into(), ttl_secs.to_string());
        if no_auto_update {
            vars.insert("HOMEBREW_NO_AUTO_UPDATE".into(), "1".into());
        }

        let input = EnvDetectInput {
            os: "linux".into(),
            arch: "x86_64".into(),
            home,
            xdg_cache_home: None,
            vars,
            available_parallelism: 2,
        };
        Env::detect_from(&input, &PanicRunner).expect("test Env")
    }

    fn http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("reqwest client")
    }

    async fn write_cache(path: &Utf8Path, bytes: &[u8], age: Duration) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir api");
        }
        fs::write(path, bytes).expect("write cache");
        let file = File::options().write(true).open(path).expect("open cache");
        let mtime = SystemTime::now().checked_sub(age).expect("mtime");
        file.set_modified(mtime).expect("set mtime");
    }

    #[test]
    fn api_warning_exact_cache_fallback_body() {
        let warning = ApiWarning::CacheFallback {
            file: "formula.jws.json".into(),
        };
        assert_eq!(
            warning.to_string(),
            "formula.jws.json: update failed, falling back to cached version."
        );
        let nested = ApiWarning::CacheFallback {
            file: "internal/packages.x86_64_linux.jws.json".into(),
        };
        assert_eq!(
            nested.to_string(),
            "packages.x86_64_linux.jws.json: update failed, falling back to cached version."
        );
    }

    #[tokio::test]
    async fn fresh_cache_verifies_and_skips_network() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 450, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &envelope("fresh-payload"), Duration::from_secs(10)).await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &FakeVerifier::new(),
            &server.uri(),
        )
        .await
        .expect("load");

        assert_eq!(loaded.payload, "fresh-payload");
        assert!(loaded.warning.is_none());
    }

    #[tokio::test]
    async fn no_auto_update_uses_cache_even_when_stale() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 1, true);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(
            &path,
            &envelope("stale-but-pinned"),
            Duration::from_secs(3600),
        )
        .await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &FakeVerifier::new(),
            &server.uri(),
        )
        .await
        .expect("load");

        assert_eq!(loaded.payload, "stale-but-pinned");
        assert!(loaded.warning.is_none());
    }

    #[tokio::test]
    async fn stale_cache_sends_if_modified_since_and_304_touches_mtime() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let env = test_env(&cache_root, &server.uri(), 1, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &envelope("cached"), Duration::from_secs(120)).await;
        let mtime_before = fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");
        let expected_ims = if_modified_since_header(&path)
            .expect("ims")
            .expect("header");

        let seen_ims = std::sync::Arc::new(Mutex::new(None::<String>));
        let seen = seen_ims.clone();
        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .and(header_exists("if-modified-since"))
            .respond_with(move |req: &Request| {
                let value = req
                    .headers
                    .get("if-modified-since")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                *seen.lock().expect("ims lock") = value;
                ResponseTemplate::new(304)
            })
            .expect(1)
            .mount(&server)
            .await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &FakeVerifier::new(),
            &server.uri(),
        )
        .await
        .expect("load");

        assert_eq!(loaded.payload, "cached");
        assert!(loaded.warning.is_none(), "304 is success, not fallback");
        let got_ims = seen_ims
            .lock()
            .expect("ims lock")
            .clone()
            .expect("IMS header");
        assert_eq!(
            got_ims, expected_ims,
            "If-Modified-Since must be IMF-fixdate mtime"
        );
        let mtime_after = fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");
        assert!(
            mtime_after > mtime_before,
            "304 must bump cache mtime (before={mtime_before:?}, after={mtime_after:?})"
        );
    }

    #[tokio::test]
    async fn stale_cache_200_writes_via_incomplete_and_returns_payload() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;
        let new_bytes = envelope("new-payload");

        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .and(header_exists("if-modified-since"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(new_bytes.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 1, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &envelope("old-payload"), Duration::from_secs(120)).await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &FakeVerifier::new(),
            &server.uri(),
        )
        .await
        .expect("load");

        assert_eq!(loaded.payload, "new-payload");
        assert_eq!(fs::read(&path).expect("read cache"), new_bytes);
        assert!(
            !incomplete_path(&path).exists(),
            "incomplete must be renamed away"
        );
    }

    #[tokio::test]
    async fn network_failure_falls_back_to_cache_with_exact_warning() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 1, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(
            &path,
            &envelope("fallback-payload"),
            Duration::from_secs(999),
        )
        .await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &FakeVerifier::new(),
            &server.uri(),
        )
        .await
        .expect("fallback");

        assert_eq!(loaded.payload, "fallback-payload");
        assert_eq!(
            loaded.warning.expect("warning").to_string(),
            "formula.jws.json: update failed, falling back to cached version."
        );
    }

    #[tokio::test]
    async fn network_failure_without_cache_returns_cannot_download_default_url() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let primary = MockServer::start().await;
        let default = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&primary)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&default)
            .await;

        let env = test_env(&cache_root, &primary.uri(), 1, false);
        let err = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &FakeVerifier::new(),
            &default.uri(),
        )
        .await
        .expect_err("must fail");

        match err {
            ApiError::CannotDownload { url } => {
                assert_eq!(url, format!("{}/formula.jws.json", default.uri()));
            }
            other => panic!("expected CannotDownload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn custom_domain_failure_retries_default_domain_once() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let primary = MockServer::start().await;
        let default = MockServer::start().await;
        let body = envelope("from-default");

        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&primary)
            .await;
        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&default)
            .await;

        let env = test_env(&cache_root, &primary.uri(), 1, false);
        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &FakeVerifier::new(),
            &default.uri(),
        )
        .await
        .expect("retry default");

        assert_eq!(loaded.payload, "from-default");
        assert!(loaded.warning.is_none());
        assert_eq!(
            fs::read(cache_path(&env, "formula.jws.json")).expect("cache"),
            body
        );
    }

    #[tokio::test]
    async fn verification_failure_deletes_cache_and_refetches_once() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let bad = envelope("bad");
        let good = envelope("good");
        let verifier = FakeVerifier::new();
        verifier.reject_once_envelope(&bad);

        let env = test_env(&cache_root, &server.uri(), 450, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &bad, Duration::from_secs(1)).await;

        let requests = std::sync::Arc::new(Mutex::new(0u32));
        let counter = requests.clone();
        let good_clone = good.clone();
        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(move |_req: &Request| {
                let mut guard = counter.lock().expect("counter");
                *guard += 1;
                ResponseTemplate::new(200).set_body_bytes(good_clone.clone())
            })
            .expect(1)
            .mount(&server)
            .await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &verifier,
            &server.uri(),
        )
        .await
        .expect("refetch");

        assert_eq!(loaded.payload, "good");
        assert_eq!(fs::read(&path).expect("cache"), good);
        assert_eq!(*requests.lock().expect("counter"), 1);
    }

    #[tokio::test]
    async fn verification_failure_exhausted_returns_cannot_download() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let bad = envelope("always-bad");
        let verifier = FakeVerifier::new();
        verifier.reject_always_envelope(&bad);

        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bad.clone()))
            .expect(1..=2)
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 1, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &bad, Duration::from_secs(999)).await;

        let err = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &verifier,
            &server.uri(),
        )
        .await
        .expect_err("signature failures must not fall back");

        match err {
            ApiError::CannotDownload { url } => {
                assert_eq!(url, format!("{}/formula.jws.json", server.uri()));
            }
            other => panic!("expected CannotDownload, got {other:?}"),
        }
        assert!(
            !path.exists() || fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true),
            "corrupt cache must be removed after exhausted refetch"
        );
    }

    #[tokio::test]
    async fn force_ignores_ttl_and_no_auto_update() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let old = envelope("old-force");
        let new = envelope("new-force");

        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(new.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 450, true);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &old, Duration::from_secs(0)).await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Force,
            &FakeVerifier::new(),
            &server.uri(),
        )
        .await
        .expect("force");

        assert_eq!(loaded.payload, "new-force");
        assert_eq!(loaded.previous.as_deref(), Some("old-force"));
        assert!(loaded.warning.is_none());
        assert_eq!(fs::read(&path).expect("cache"), new);
    }

    #[tokio::test]
    async fn force_rejects_unverified_download_and_preserves_old_cache() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let old = envelope("keep-me");
        let bad = envelope("evil");
        let verifier = FakeVerifier::new();
        verifier.reject_always_envelope(&bad);

        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bad.clone()))
            .expect(1..=2)
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 450, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &old, Duration::from_secs(0)).await;

        let err = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Force,
            &verifier,
            &server.uri(),
        )
        .await
        .expect_err("unverified must not install");

        assert!(matches!(err, ApiError::CannotDownload { .. }));
        assert_eq!(
            fs::read(&path).expect("old cache preserved"),
            old,
            "Force must not replace verified cache with unverified bytes"
        );
        assert!(
            !incomplete_path(&path).exists(),
            "incomplete artifact must not remain"
        );
    }

    #[tokio::test]
    async fn stale_good_cache_survives_bad_download_then_network_fallback() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let good = envelope("still-good");
        let bad = envelope("bad-body");
        let verifier = FakeVerifier::new();
        verifier.reject_always_envelope(&bad);

        let calls = std::sync::Arc::new(Mutex::new(0u32));
        let counter = calls.clone();
        let bad_clone = bad.clone();
        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(move |_req: &Request| {
                let mut guard = counter.lock().expect("counter");
                *guard += 1;
                if *guard == 1 {
                    ResponseTemplate::new(200).set_body_bytes(bad_clone.clone())
                } else {
                    ResponseTemplate::new(500)
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 1, false);
        let path = cache_path(&env, "formula.jws.json");
        write_cache(&path, &good, Duration::from_secs(999)).await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "formula.jws.json",
            CacheMode::Normal,
            &verifier,
            &server.uri(),
        )
        .await
        .expect("fallback to preserved cache");

        assert_eq!(loaded.payload, "still-good");
        assert_eq!(
            loaded.warning.expect("warning").to_string(),
            "formula.jws.json: update failed, falling back to cached version."
        );
        assert_eq!(fs::read(&path).expect("preserved"), good);
        assert_eq!(*calls.lock().expect("counter"), 2);
    }

    #[tokio::test]
    async fn force_network_failure_returns_old_payload_with_warning() {
        let tmp = TempDir::new().expect("tempdir");
        let cache_root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let env = test_env(&cache_root, &server.uri(), 450, false);
        let path = cache_path(&env, "cask.jws.json");
        write_cache(&path, &envelope("force-fallback"), Duration::from_secs(0)).await;

        let loaded = load_signed_with(
            &env,
            &http_client(),
            "cask.jws.json",
            CacheMode::Force,
            &FakeVerifier::new(),
            &server.uri(),
        )
        .await
        .expect("force fallback");

        assert_eq!(loaded.payload, "force-fallback");
        assert_eq!(
            loaded.warning.expect("warning").to_string(),
            "cask.jws.json: update failed, falling back to cached version."
        );
    }

    #[test]
    fn default_api_domain_is_homebrew_formulae() {
        assert_eq!(DEFAULT_API_DOMAIN, "https://formulae.brew.sh/api");
        assert_eq!(
            join_url(DEFAULT_API_DOMAIN, "formula.jws.json"),
            "https://formulae.brew.sh/api/formula.jws.json"
        );
    }
}
