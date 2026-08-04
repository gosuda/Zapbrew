//! In-memory catalogs over the verified Homebrew JSON API payloads.
//!
//! [`Catalog`] and [`CaskCatalog`] compose the crate's lower seams:
//! [`crate::jws`] (production key verification), [`crate::transport`] (fetch,
//! verify, cache), and [`crate::model`] (variation merge + typed parse). They
//! add deterministic name indexing on top:
//!
//! - formula lookup resolves exact name, then alias, then oldname;
//! - a `Missing` resolution suggests the two closest names by Jaro-Winkler
//!   similarity above `0.8`;
//! - cask lookup resolves token, then a migrated `old_tokens` rename.
//!
//! Production [`Catalog::load`] / [`CaskCatalog::load`] / [`force_refresh`]
//! construct the embedded-key [`HomebrewVerifier`]; the `from_payload`
//! constructors index already-verified bytes for deterministic offline use and
//! tests without touching the network or verification.

use std::collections::{HashMap, HashSet};

use jaro_winkler::jaro_winkler;
use serde_json::Value;
use zapbrew_prefix::Env;
use zapbrew_types::BottleTag;

use crate::error::ApiError;
use crate::jws::{HomebrewVerifier, JwsVerifier};
use crate::model::{self, Cask, Formula};
use crate::transport::{self, ApiWarning, CacheMode};

/// Signed catalog of every formula, keyed by name.
const FORMULA_ENDPOINT: &str = "formula.jws.json";
/// Signed catalog of every cask, keyed by token.
const CASK_ENDPOINT: &str = "cask.jws.json";
/// Lazily fetched oldname -> new-tap map for migration hints.
const FORMULA_TAP_MIGRATIONS_ENDPOINT: &str = "formula_tap_migrations.jws.json";

/// Minimum Jaro-Winkler similarity for a did-you-mean suggestion (strict `>`).
const SUGGESTION_THRESHOLD: f32 = 0.8;
/// Maximum did-you-mean suggestions surfaced on a `Missing` resolution.
const MAX_SUGGESTIONS: usize = 2;

/// Outcome of resolving a requested formula name against the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The name is an exact formula name.
    Exact,
    /// The name is an alias of `real`.
    Alias {
        /// The canonical formula name the alias points at.
        real: String,
    },
    /// The name is a former name (`oldnames`) of `new`.
    Oldname {
        /// The current formula name the oldname was renamed to.
        new: String,
    },
    /// No name, alias, or oldname matched; `did_you_mean` holds the closest
    /// names (at most two), best first.
    Missing {
        /// Suggested names, ordered by descending similarity then name.
        did_you_mean: Vec<String>,
    },
}

/// The formula catalog: every formula plus its name, alias, and oldname index.
pub struct Catalog {
    formulae: Vec<Formula>,
    by_name: HashMap<String, usize>,
    aliases: HashMap<String, String>,
    oldnames: HashMap<String, String>,
    warnings: Vec<ApiWarning>,
}

impl Catalog {
    /// Load and index the formula catalog, honoring the cache TTL.
    ///
    /// Uses the embedded Homebrew key for verification. Any offline-fallback
    /// warning raised by the transport layer is retained (see [`warnings`]).
    ///
    /// [`warnings`]: Catalog::warnings
    pub async fn load(env: &Env, http: &reqwest::Client) -> Result<Self, ApiError> {
        let verifier = HomebrewVerifier::new()?;
        Self::load_with(env, http, &verifier).await
    }

    async fn load_with(
        env: &Env,
        http: &reqwest::Client,
        verifier: &dyn JwsVerifier,
    ) -> Result<Self, ApiError> {
        let loaded =
            transport::load_signed(env, http, FORMULA_ENDPOINT, CacheMode::Normal, verifier)
                .await?;
        let formulae = model::parse_formulae(loaded.payload.as_bytes(), &env.bottle_tag)?;
        Ok(Self::index(formulae, loaded.warning.into_iter().collect()))
    }

    /// Index an already-verified `formula.jws.json` payload for a bottle tag.
    ///
    /// Deterministic and I/O-free: no network, no signature verification. The
    /// caller supplies verified bytes (e.g. ops fixtures or a payload the
    /// transport layer already returned).
    pub fn from_payload(payload: &[u8], tag: &BottleTag) -> Result<Self, ApiError> {
        let formulae = model::parse_formulae(payload, tag)?;
        Ok(Self::index(formulae, Vec::new()))
    }

    /// Look up a formula by exact name, then alias, then oldname.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Formula> {
        if let Some(&idx) = self.by_name.get(name) {
            return self.formulae.get(idx);
        }
        if let Some(real) = self.aliases.get(name)
            && let Some(&idx) = self.by_name.get(real)
        {
            return self.formulae.get(idx);
        }
        if let Some(new) = self.oldnames.get(name)
            && let Some(&idx) = self.by_name.get(new)
        {
            return self.formulae.get(idx);
        }
        None
    }

    /// Classify a requested name as exact, alias, oldname, or missing.
    ///
    /// Precedence matches [`get`]: an exact name wins over an alias, which wins
    /// over an oldname. A miss carries the closest suggestions.
    ///
    /// [`get`]: Catalog::get
    #[must_use]
    pub fn resolve(&self, name: &str) -> Resolution {
        if self.by_name.contains_key(name) {
            Resolution::Exact
        } else if let Some(real) = self.aliases.get(name) {
            Resolution::Alias { real: real.clone() }
        } else if let Some(new) = self.oldnames.get(name) {
            Resolution::Oldname { new: new.clone() }
        } else {
            Resolution::Missing {
                did_you_mean: self.suggestions(name),
            }
        }
    }

    /// Iterate over every formula in payload order.
    pub fn iter(&self) -> impl Iterator<Item = &Formula> {
        self.formulae.iter()
    }

    /// Typed warnings raised while loading (e.g. offline cache fallback).
    ///
    /// Empty for [`from_payload`]. Ops render these with the CLI's `Warning: `
    /// prefix without this crate depending on any output seam.
    ///
    /// [`from_payload`]: Catalog::from_payload
    #[must_use]
    pub fn warnings(&self) -> &[ApiWarning] {
        &self.warnings
    }

    /// Look up the tap a missing formula was migrated to, if any.
    ///
    /// `formula_tap_migrations.jws.json` is fetched and verified lazily (TTL
    /// honored). Callers invoke this after a [`Resolution::Missing`] to build
    /// the `Error: <name> was migrated to <tap>` hint. Returns `Ok(None)` when
    /// the name is not in the migration map; fetch/verify errors surface.
    pub async fn migration_hint(
        &self,
        env: &Env,
        http: &reqwest::Client,
        name: &str,
    ) -> Result<Option<String>, ApiError> {
        let verifier = HomebrewVerifier::new()?;
        self.migration_hint_with(env, http, &verifier, name).await
    }

    async fn migration_hint_with(
        &self,
        env: &Env,
        http: &reqwest::Client,
        verifier: &dyn JwsVerifier,
        name: &str,
    ) -> Result<Option<String>, ApiError> {
        let loaded = transport::load_signed(
            env,
            http,
            FORMULA_TAP_MIGRATIONS_ENDPOINT,
            CacheMode::Normal,
            verifier,
        )
        .await?;
        parse_migration_target(loaded.payload.as_bytes(), name)
    }

    fn index(formulae: Vec<Formula>, warnings: Vec<ApiWarning>) -> Self {
        let mut by_name = HashMap::with_capacity(formulae.len());
        let mut aliases = HashMap::new();
        let mut oldnames = HashMap::new();
        for (idx, formula) in formulae.iter().enumerate() {
            by_name.insert(formula.name.clone(), idx);
            for alias in &formula.aliases {
                aliases.insert(alias.clone(), formula.name.clone());
            }
            for oldname in &formula.oldnames {
                oldnames.insert(oldname.clone(), formula.name.clone());
            }
        }
        Self {
            formulae,
            by_name,
            aliases,
            oldnames,
            warnings,
        }
    }

    fn suggestions(&self, name: &str) -> Vec<String> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut scored: Vec<(f32, &str)> = Vec::new();
        for candidate in self.by_name.keys().chain(self.aliases.keys()) {
            if !seen.insert(candidate.as_str()) {
                continue;
            }
            let score = jaro_winkler(name, candidate);
            if score > SUGGESTION_THRESHOLD {
                scored.push((score, candidate.as_str()));
            }
        }
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(b.1))
        });
        scored
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(_, candidate)| candidate.to_owned())
            .collect()
    }
}

/// The cask catalog: every cask plus its token and `old_tokens` rename index.
///
/// Casks are metadata-only here (search/info + Step 6 install dispatch); each
/// [`Cask`] retains its raw JSON for `info --json=v2` passthrough.
pub struct CaskCatalog {
    casks: Vec<Cask>,
    by_token: HashMap<String, usize>,
    renames: HashMap<String, String>,
    warnings: Vec<ApiWarning>,
}

impl CaskCatalog {
    /// Load and index the cask catalog, honoring the cache TTL.
    pub async fn load(env: &Env, http: &reqwest::Client) -> Result<Self, ApiError> {
        let verifier = HomebrewVerifier::new()?;
        Self::load_with(env, http, &verifier).await
    }

    async fn load_with(
        env: &Env,
        http: &reqwest::Client,
        verifier: &dyn JwsVerifier,
    ) -> Result<Self, ApiError> {
        let loaded =
            transport::load_signed(env, http, CASK_ENDPOINT, CacheMode::Normal, verifier).await?;
        let casks = model::parse_casks(loaded.payload.as_bytes(), &env.bottle_tag)?;
        Ok(Self::index(casks, loaded.warning.into_iter().collect()))
    }

    /// Index an already-verified `cask.jws.json` payload for a bottle tag.
    ///
    /// Deterministic and I/O-free, mirroring [`Catalog::from_payload`].
    pub fn from_payload(payload: &[u8], tag: &BottleTag) -> Result<Self, ApiError> {
        let casks = model::parse_casks(payload, tag)?;
        Ok(Self::index(casks, Vec::new()))
    }

    /// Look up a cask by exact token, then by a migrated `old_tokens` rename.
    #[must_use]
    pub fn get(&self, token: &str) -> Option<&Cask> {
        if let Some(&idx) = self.by_token.get(token) {
            return self.casks.get(idx);
        }
        if let Some(real) = self.renames.get(token)
            && let Some(&idx) = self.by_token.get(real)
        {
            return self.casks.get(idx);
        }
        None
    }

    /// Iterate over every cask in payload order.
    pub fn iter(&self) -> impl Iterator<Item = &Cask> {
        self.casks.iter()
    }

    /// Typed warnings raised while loading (e.g. offline cache fallback).
    #[must_use]
    pub fn warnings(&self) -> &[ApiWarning] {
        &self.warnings
    }

    fn index(casks: Vec<Cask>, warnings: Vec<ApiWarning>) -> Self {
        let mut by_token = HashMap::with_capacity(casks.len());
        let mut renames = HashMap::new();
        for (idx, cask) in casks.iter().enumerate() {
            by_token.insert(cask.token.clone(), idx);
            for old_token in &cask.old_tokens {
                renames.insert(old_token.clone(), cask.token.clone());
            }
        }
        Self {
            casks,
            by_token,
            renames,
            warnings,
        }
    }
}

/// Result of a forced catalog refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshReport {
    /// Count of formula names whose raw JSON object changed versus the payload
    /// cached before the refresh (added or modified; removed names excluded).
    pub formulae_changed: usize,
    /// Whether a prior verified cask payload existed and differed from the
    /// newly verified payload. A first fetch is not reported as changed.
    pub casks_changed: bool,
    /// Offline fallback warnings raised while refreshing either catalog.
    pub warnings: Vec<ApiWarning>,
}

/// Force-refresh both the formula and cask payloads, ignoring the TTL.
///
/// Mirrors `brew update`'s API refresh. The transport layer preserves each
/// verified on-disk cache until a verified replacement is written, so a failed
/// refresh never corrupts the cache and returns the prior verified formula
/// payload as `previous`, letting [`RefreshReport::formulae_changed`] count
/// changed objects and [`RefreshReport::casks_changed`] compare the cask
/// payload. When no prior verified cache existed, neither payload is reported
/// as changed.
pub async fn force_refresh(env: &Env, http: &reqwest::Client) -> Result<RefreshReport, ApiError> {
    let verifier = HomebrewVerifier::new()?;
    force_refresh_with(env, http, &verifier).await
}

async fn force_refresh_with(
    env: &Env,
    http: &reqwest::Client,
    verifier: &dyn JwsVerifier,
) -> Result<RefreshReport, ApiError> {
    let formula =
        transport::load_signed(env, http, FORMULA_ENDPOINT, CacheMode::Force, verifier).await?;
    // Step 6 `update` refreshes both payloads; refresh the cask cache too.
    let cask = transport::load_signed(env, http, CASK_ENDPOINT, CacheMode::Force, verifier).await?;

    let warnings = collect_refresh_warnings(formula.warning, cask.warning);
    let formulae_changed = match formula.previous {
        Some(previous) => count_changed_formulae(&previous, &formula.payload)?,
        None => 0,
    };
    let casks_changed = cask
        .previous
        .as_deref()
        .is_some_and(|previous| previous != cask.payload);
    Ok(RefreshReport {
        formulae_changed,
        casks_changed,
        warnings,
    })
}

fn collect_refresh_warnings(
    formula: Option<ApiWarning>,
    cask: Option<ApiWarning>,
) -> Vec<ApiWarning> {
    [formula, cask].into_iter().flatten().collect()
}

/// Count formula names whose raw JSON object differs from `previous`.
///
/// A name present in `current` but absent from `previous` counts as changed
/// (its object appeared); a name only in `previous` (removed) is not counted.
fn count_changed_formulae(previous: &str, current: &str) -> Result<usize, ApiError> {
    let old = parse_formula_array(previous, "previous formula payload")?;
    let new = parse_formula_array(current, "formula payload")?;
    let old_by_name: HashMap<&str, &Value> = old
        .iter()
        .filter_map(|obj| Some((obj.get("name")?.as_str()?, obj)))
        .collect();
    let mut changed = 0;
    for obj in &new {
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            continue;
        };
        match old_by_name.get(name) {
            Some(prev) if *prev == obj => {}
            _ => changed += 1,
        }
    }
    Ok(changed)
}

fn parse_formula_array(payload: &str, context: &str) -> Result<Vec<Value>, ApiError> {
    serde_json::from_str(payload).map_err(|source| ApiError::Json {
        context: context.to_owned(),
        source,
    })
}

fn parse_migration_target(payload: &[u8], name: &str) -> Result<Option<String>, ApiError> {
    let migrations: HashMap<String, Value> =
        serde_json::from_slice(payload).map_err(|source| ApiError::Json {
            context: "formula_tap_migrations".to_owned(),
            source,
        })?;
    Ok(migrations
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap as StdHashMap;
    use std::fs::{self, File};
    use std::time::{Duration, SystemTime};

    use camino::{Utf8Path, Utf8PathBuf};
    use tempfile::TempDir;
    use wiremock::matchers::{method, path as url_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, EnvDetectInput};

    // --- helpers -----------------------------------------------------------

    fn tag() -> BottleTag {
        "x86_64_linux"
            .parse::<BottleTag>()
            .expect("x86_64_linux is a valid bottle tag")
    }

    /// A JWS envelope whose `payload` string is `payload`. The test verifier
    /// returns that string verbatim; no real crypto is involved.
    fn envelope(payload: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "payload": payload,
            "signatures": [{"protected": "e30", "header": {"kid": "test"}, "signature": "AA"}],
        }))
        .expect("envelope json")
    }

    /// Envelope -> payload extractor implementing the crate-private verifier
    /// seam without a production key (mirrors the transport test harness).
    struct FakeVerifier;

    impl JwsVerifier for FakeVerifier {
        fn verify(&self, envelope: &[u8]) -> Result<Vec<u8>, ApiError> {
            let value: Value =
                serde_json::from_slice(envelope).map_err(|source| ApiError::Json {
                    context: "jws envelope".to_owned(),
                    source,
                })?;
            let payload = value
                .get("payload")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::invalid("jws envelope", "missing string payload"))?;
            Ok(payload.as_bytes().to_vec())
        }
    }

    struct PanicRunner;

    impl CommandRunner for PanicRunner {
        fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
            panic!("Linux Env::detect_from must not spawn processes");
        }
    }

    fn test_env(cache: &Utf8Path, api_domain: &str) -> Env {
        let home = cache
            .parent()
            .map(Utf8Path::to_path_buf)
            .unwrap_or_else(|| cache.to_path_buf());
        let prefix = home.join("prefix");
        let mut vars = StdHashMap::new();
        vars.insert("HOMEBREW_PREFIX".into(), prefix.to_string());
        vars.insert("HOMEBREW_CACHE".into(), cache.to_string());
        vars.insert("HOMEBREW_API_DOMAIN".into(), api_domain.to_owned());
        vars.insert("HOMEBREW_API_AUTO_UPDATE_SECS".into(), "450".into());

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

    fn cache_file(env: &Env, file: &str) -> Utf8PathBuf {
        env.cache.join("api").join(file)
    }

    fn write_cache(path: &Utf8Path, bytes: &[u8], age: Duration) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir api");
        }
        fs::write(path, bytes).expect("write cache");
        let file = File::options().write(true).open(path).expect("open cache");
        let mtime = SystemTime::now().checked_sub(age).expect("mtime");
        file.set_modified(mtime).expect("set mtime");
    }

    async fn refresh_cask(previous: Option<&str>, current: &str) -> RefreshReport {
        let tmp = TempDir::new().expect("tempdir");
        let cache = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;
        let env = test_env(&cache, &server.uri());
        if let Some(payload) = previous {
            write_cache(
                &cache_file(&env, "cask.jws.json"),
                &envelope(payload),
                Duration::from_secs(3600),
            );
        }

        for (endpoint, payload) in [("/formula.jws.json", "[]"), ("/cask.jws.json", current)] {
            Mock::given(method("GET"))
                .and(url_path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(envelope(payload)))
                .expect(1)
                .mount(&server)
                .await;
        }

        force_refresh_with(&env, &http_client(), &FakeVerifier)
            .await
            .expect("force refresh")
    }

    // --- deterministic index / lookup (no network) -------------------------

    fn sample_formula_payload() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!([
            { "name": "foo", "aliases": ["foo-alias"], "oldnames": ["foo-old"] },
            { "name": "bar" },
            { "name": "baz", "aliases": ["bar"] },
        ]))
        .expect("payload json")
    }

    #[test]
    fn indexes_exact_alias_and_oldname_with_precedence() {
        let catalog =
            Catalog::from_payload(&sample_formula_payload(), &tag()).expect("catalog parses");

        assert_eq!(catalog.resolve("foo"), Resolution::Exact);
        assert_eq!(catalog.get("foo").map(|f| f.name.as_str()), Some("foo"));

        assert_eq!(
            catalog.resolve("foo-alias"),
            Resolution::Alias {
                real: "foo".to_owned()
            }
        );
        assert_eq!(
            catalog.get("foo-alias").map(|f| f.name.as_str()),
            Some("foo")
        );

        assert_eq!(
            catalog.resolve("foo-old"),
            Resolution::Oldname {
                new: "foo".to_owned()
            }
        );
        assert_eq!(catalog.get("foo-old").map(|f| f.name.as_str()), Some("foo"));

        // "bar" is a real formula and also declared as baz's alias; the exact
        // name wins for both resolve and get.
        assert_eq!(catalog.resolve("bar"), Resolution::Exact);
        assert_eq!(catalog.get("bar").map(|f| f.name.as_str()), Some("bar"));

        assert_eq!(catalog.get("does-not-exist"), None);
    }

    #[test]
    fn from_payload_has_no_warnings() {
        let catalog =
            Catalog::from_payload(&sample_formula_payload(), &tag()).expect("catalog parses");
        assert!(catalog.warnings().is_empty());
        assert_eq!(catalog.iter().count(), 3);
    }

    #[test]
    fn missing_suggests_top_two_above_threshold_sorted() {
        // Four names; a query close to three of them, far from "zzzzzzzz".
        let payload = serde_json::to_vec(&serde_json::json!([
            { "name": "package" },
            { "name": "packages" },
            { "name": "packaged" },
            { "name": "zzzzzzzz" },
        ]))
        .expect("payload json");
        let catalog = Catalog::from_payload(&payload, &tag()).expect("catalog parses");

        let query = "packagd";
        let Resolution::Missing { did_you_mean } = catalog.resolve(query) else {
            panic!("expected a missing resolution for {query}");
        };

        // Independent oracle: same threshold + cap + ordering over the names.
        let mut expected: Vec<(f32, &str)> = ["package", "packages", "packaged", "zzzzzzzz"]
            .into_iter()
            .map(|name| (jaro_winkler(query, name), name))
            .filter(|(score, _)| *score > SUGGESTION_THRESHOLD)
            .collect();
        expected.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(b.1))
        });
        let expected: Vec<String> = expected
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(_, name)| name.to_owned())
            .collect();

        assert_eq!(did_you_mean, expected);
        assert!(did_you_mean.len() <= MAX_SUGGESTIONS);
        assert!(!did_you_mean.iter().any(|s| s == "zzzzzzzz"));
        for suggestion in &did_you_mean {
            assert!(jaro_winkler(query, suggestion) > SUGGESTION_THRESHOLD);
        }
    }

    #[test]
    fn missing_far_query_suggests_nothing() {
        let catalog =
            Catalog::from_payload(&sample_formula_payload(), &tag()).expect("catalog parses");
        assert_eq!(
            catalog.resolve("qwertyuiop-nowhere"),
            Resolution::Missing {
                did_you_mean: Vec::new()
            }
        );
    }

    #[test]
    fn warnings_surface_typed_cache_fallback() {
        let catalog = Catalog::index(
            Vec::new(),
            vec![ApiWarning::CacheFallback {
                file: "formula.jws.json".to_owned(),
            }],
        );
        assert_eq!(catalog.warnings().len(), 1);
        assert_eq!(
            catalog.warnings()[0].to_string(),
            "formula.jws.json: update failed, falling back to cached version."
        );
    }

    // --- cask index / lookup ----------------------------------------------

    #[test]
    fn cask_catalog_indexes_token_and_rename() {
        let payload = serde_json::to_vec(&serde_json::json!([
            { "token": "firefox", "old_tokens": ["fx"] },
            { "token": "chrome" },
        ]))
        .expect("payload json");
        let catalog = CaskCatalog::from_payload(&payload, &tag()).expect("cask catalog parses");

        assert_eq!(
            catalog.get("firefox").map(|c| c.token.as_str()),
            Some("firefox")
        );
        assert_eq!(
            catalog.get("chrome").map(|c| c.token.as_str()),
            Some("chrome")
        );
        // Renamed token resolves to its current cask.
        assert_eq!(catalog.get("fx").map(|c| c.token.as_str()), Some("firefox"));
        assert_eq!(catalog.get("missing"), None);
        assert_eq!(catalog.iter().count(), 2);
    }

    // --- force change counting (pure) -------------------------------------

    #[test]
    fn count_changed_counts_modified_and_added_not_removed() {
        let previous = serde_json::json!([
            { "name": "a", "v": 1 },
            { "name": "b", "v": 1 },
            { "name": "c", "v": 1 },
        ])
        .to_string();
        let current = serde_json::json!([
            { "name": "a", "v": 1 },      // unchanged
            { "name": "b", "v": 2 },      // modified
            { "name": "d", "v": 1 },      // added ("c" removed, not counted)
        ])
        .to_string();

        assert_eq!(
            count_changed_formulae(&previous, &current).expect("count"),
            2
        );
    }

    #[test]
    fn count_changed_identical_payloads_is_zero() {
        let payload = serde_json::json!([{ "name": "a", "v": 1 }]).to_string();
        assert_eq!(
            count_changed_formulae(&payload, &payload).expect("count"),
            0
        );
    }

    #[test]
    fn refresh_report_keeps_both_catalog_warnings() {
        let warnings = collect_refresh_warnings(
            Some(ApiWarning::CacheFallback {
                file: "formula.jws.json".to_owned(),
            }),
            Some(ApiWarning::CacheFallback {
                file: "cask.jws.json".to_owned(),
            }),
        );
        assert_eq!(
            warnings.iter().map(ApiWarning::message).collect::<Vec<_>>(),
            [
                "formula.jws.json: update failed, falling back to cached version.",
                "cask.jws.json: update failed, falling back to cached version.",
            ]
        );
    }

    // --- migration target parsing (pure) ----------------------------------

    #[test]
    fn migration_target_extracts_named_entry_only() {
        let payload = serde_json::json!({
            "old-formula": "homebrew/foo",
            "other": "someuser/bar/renamed",
        })
        .to_string();

        assert_eq!(
            parse_migration_target(payload.as_bytes(), "old-formula").expect("parse"),
            Some("homebrew/foo".to_owned())
        );
        assert_eq!(
            parse_migration_target(payload.as_bytes(), "absent").expect("parse"),
            None
        );
    }

    // --- wiremock end-to-end (load, warnings, force, migration) ------------

    #[tokio::test]
    async fn load_fetches_verifies_and_indexes() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let payload =
            serde_json::json!([{ "name": "wget", "aliases": ["wget-alias"] }]).to_string();
        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(envelope(&payload)))
            .expect(1)
            .mount(&server)
            .await;

        let env = test_env(&cache, &server.uri());
        let catalog = Catalog::load_with(&env, &http_client(), &FakeVerifier)
            .await
            .expect("load");

        assert_eq!(catalog.get("wget").map(|f| f.name.as_str()), Some("wget"));
        assert_eq!(
            catalog.resolve("wget-alias"),
            Resolution::Alias {
                real: "wget".to_owned()
            }
        );
        assert!(catalog.warnings().is_empty());
    }

    #[tokio::test]
    async fn cask_catalog_loads_over_network() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let payload =
            serde_json::json!([{ "token": "iterm2", "old_tokens": ["iterm"] }]).to_string();
        Mock::given(method("GET"))
            .and(url_path("/cask.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(envelope(&payload)))
            .expect(1)
            .mount(&server)
            .await;

        let env = test_env(&cache, &server.uri());
        let catalog = CaskCatalog::load_with(&env, &http_client(), &FakeVerifier)
            .await
            .expect("cask load");

        assert_eq!(
            catalog.get("iterm2").map(|c| c.token.as_str()),
            Some("iterm2")
        );
        assert_eq!(
            catalog.get("iterm").map(|c| c.token.as_str()),
            Some("iterm2")
        );
    }

    #[tokio::test]
    async fn force_refresh_counts_changed_formulae() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let env = test_env(&cache, &server.uri());

        // Old verified formula payload sits in the cache before the refresh.
        let old_payload = serde_json::json!([
            { "name": "a", "v": 1 },
            { "name": "b", "v": 1 },
        ])
        .to_string();
        write_cache(
            &cache_file(&env, "formula.jws.json"),
            &envelope(&old_payload),
            Duration::from_secs(3600),
        );

        // Force fetches the new formula payload and refreshes the cask cache.
        let new_payload = serde_json::json!([
            { "name": "a", "v": 1 },      // unchanged
            { "name": "b", "v": 2 },      // modified
            { "name": "c", "v": 1 },      // added
        ])
        .to_string();
        Mock::given(method("GET"))
            .and(url_path("/formula.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(envelope(&new_payload)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(url_path("/cask.jws.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(envelope(&serde_json::json!([]).to_string())),
            )
            .mount(&server)
            .await;

        let report = force_refresh_with(&env, &http_client(), &FakeVerifier)
            .await
            .expect("force refresh");

        assert_eq!(report.formulae_changed, 2);
        assert!(!report.casks_changed);
    }

    #[tokio::test]
    async fn force_refresh_does_not_mark_an_unchanged_cask_payload() {
        let payload = serde_json::json!([{ "token": "iterm2" }]).to_string();

        let report = refresh_cask(Some(&payload), &payload).await;

        assert!(!report.casks_changed);
    }

    #[tokio::test]
    async fn force_refresh_marks_a_changed_cask_payload() {
        let previous = serde_json::json!([{ "token": "iterm2", "version": "1" }]).to_string();
        let current = serde_json::json!([{ "token": "iterm2", "version": "2" }]).to_string();

        let report = refresh_cask(Some(&previous), &current).await;

        assert!(report.casks_changed);
    }

    #[tokio::test]
    async fn force_refresh_does_not_mark_a_first_cask_fetch() {
        let current = serde_json::json!([{ "token": "iterm2" }]).to_string();

        let report = refresh_cask(None, &current).await;

        assert!(!report.casks_changed);
    }

    #[tokio::test]
    async fn migration_hint_surfaces_verified_target() {
        let tmp = TempDir::new().expect("tempdir");
        let cache = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8");
        let server = MockServer::start().await;

        let migrations = serde_json::json!({ "gone": "homebrew/cask" }).to_string();
        Mock::given(method("GET"))
            .and(url_path("/formula_tap_migrations.jws.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(envelope(&migrations)))
            .mount(&server)
            .await;

        let env = test_env(&cache, &server.uri());
        let catalog = Catalog::from_payload(&sample_formula_payload(), &tag()).expect("catalog");

        let hit = catalog
            .migration_hint_with(&env, &http_client(), &FakeVerifier, "gone")
            .await
            .expect("migration hint");
        assert_eq!(hit, Some("homebrew/cask".to_owned()));

        let miss = catalog
            .migration_hint_with(&env, &http_client(), &FakeVerifier, "still-here")
            .await
            .expect("migration hint");
        assert_eq!(miss, None);
    }
}
