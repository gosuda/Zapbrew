//! Typed Homebrew JSON-API model and the payload parsers Task 6 consumes.
//!
//! The public [`Formula`] and [`Cask`] records expose every field the verbs
//! need (versions, bottles, tagged dependencies, keg-only/deprecated/disabled
//! state, conflicts, caveats, service, cask artifacts, ...) and additionally
//! retain the raw [`serde_json::Value`] they were parsed from for
//! `info --json=v2` passthrough and `force_refresh` change detection.
//!
//! [`parse_formulae`] / [`parse_casks`] accept a raw JWS payload (the verified
//! bytes produced by the transport layer). For each entry they apply the
//! platform variation merge for the requested [`BottleTag`] — the matching
//! `variations[<tag>]` object replaces its top-level keys *wholesale* (arrays
//! are replaced, never appended) and the `variations` key is then removed — so
//! the retained raw value and the typed fields agree and reflect exactly what
//! this host sees. Deserialization is permissive: unknown API fields are
//! ignored, and entries with malformed bottle tags / checksums drop only the
//! offending bottle file rather than failing the whole catalog.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value};
use zapbrew_types::{BottleFile, BottleTag, Checksum, PkgVersion, Version};

use crate::error::ApiError;

/// A formula as served by `formula.jws.json`, merged for one host tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub tap: Option<String>,
    pub oldnames: Vec<String>,
    pub aliases: Vec<String>,
    pub desc: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    /// Stable version plus formula revision (`versions.stable` + `revision`).
    pub pkg_version: PkgVersion,
    pub revision: u32,
    pub version_scheme: u32,
    /// True when the JSON declared a bottle (`versions.bottle`).
    pub bottle_defined: bool,
    pub bottle: Option<Bottle>,
    pub pour_bottle_only_if: Option<String>,
    pub keg_only: bool,
    pub keg_only_reason: Option<KegOnlyReason>,
    /// Regular, build, test, recommended and optional dependencies, in that
    /// group order, each carrying its tags.
    pub dependencies: Vec<Dependency>,
    /// `uses_from_macos` entries with their tags and optional `since` bound.
    pub uses_from_macos: Vec<UsesFromMacos>,
    pub conflicts_with: Vec<Conflict>,
    pub link_overwrite: Vec<String>,
    pub caveats: Option<String>,
    pub deprecated: bool,
    pub deprecation_reason: Option<String>,
    pub disabled: bool,
    pub disable_reason: Option<String>,
    pub ruby_source_path: Option<String>,
    pub tap_git_head: Option<String>,
    pub post_install_defined: bool,
    /// Structured `service` object, retained verbatim for the services verb.
    pub service: Option<Value>,
    /// The merged JSON object this formula was parsed from (variations applied
    /// and removed).
    pub raw: Value,
}

impl Formula {
    /// The bottle file for `tag`, if a bottle for it was declared.
    #[must_use]
    pub fn bottle_file(&self, tag: &BottleTag) -> Option<&BottleFile> {
        self.bottle.as_ref().and_then(|b| b.file_for(tag))
    }
}

/// A formula's stable bottle block (`bottle.stable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bottle {
    pub rebuild: u32,
    pub root_url: String,
    /// Bottle files, sorted deterministically by tag spelling.
    pub files: Vec<BottleFile>,
}

impl Bottle {
    /// The declared bottle file for `tag`.
    #[must_use]
    pub fn file_for(&self, tag: &BottleTag) -> Option<&BottleFile> {
        self.files.iter().find(|f| &f.tag == tag)
    }
}

/// The `keg_only_reason` object (`{reason, explanation}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KegOnlyReason {
    pub reason: String,
    pub explanation: String,
}

/// A dependency tag carried by a [`Dependency`] or [`UsesFromMacos`] entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyTag {
    Build,
    Test,
    Recommended,
    Optional,
}

/// A single declared dependency and its tags. Regular runtime dependencies
/// carry no tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub tags: Vec<DependencyTag>,
}

impl Dependency {
    #[must_use]
    pub fn has(&self, tag: DependencyTag) -> bool {
        self.tags.contains(&tag)
    }

    #[must_use]
    pub fn is_build(&self) -> bool {
        self.has(DependencyTag::Build)
    }

    #[must_use]
    pub fn is_test(&self) -> bool {
        self.has(DependencyTag::Test)
    }

    #[must_use]
    pub fn is_recommended(&self) -> bool {
        self.has(DependencyTag::Recommended)
    }

    #[must_use]
    pub fn is_optional(&self) -> bool {
        self.has(DependencyTag::Optional)
    }

    /// A plain runtime dependency: not build-, test- or optional-only.
    #[must_use]
    pub fn is_required_runtime(&self) -> bool {
        !self.is_build() && !self.is_test() && !self.is_optional()
    }
}

/// A `uses_from_macos` entry: a system dependency on macOS, real elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsesFromMacos {
    pub name: String,
    pub tags: Vec<DependencyTag>,
    /// The `since:` macOS bound from the positional `uses_from_macos_bounds`
    /// array, if any (e.g. `"catalina"`).
    pub since: Option<String>,
}

/// A `conflicts_with` entry paired with its positional reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub name: String,
    pub reason: Option<String>,
}

/// A cask as served by `cask.jws.json`, merged for one host tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Cask {
    pub token: String,
    pub old_tokens: Vec<String>,
    pub name: Vec<String>,
    pub desc: Option<String>,
    pub homepage: Option<String>,
    pub version: Option<String>,
    /// The declared download checksum; may be the literal `"no_check"`.
    pub sha256: Option<String>,
    pub url: Option<String>,
    /// Artifact directives in declaration order.
    pub artifacts: Vec<CaskArtifact>,
    pub depends_on: CaskDependsOn,
    pub caveats: Option<String>,
    pub auto_updates: bool,
    pub deprecated: bool,
    pub disabled: bool,
    pub disable_reason: Option<String>,
    /// The merged JSON object this cask was parsed from.
    pub raw: Value,
}

/// One cask artifact directive: its kind (`app`, `binary`, `uninstall`,
/// `zap`, `installer`, ...) and the whole artifact object verbatim.
///
/// Keeping the raw value lets Task 6 dispatch on `kind` and interpret each
/// stanza, and fail closed on any unrecognized kind without losing data.
#[derive(Debug, Clone, PartialEq)]
pub struct CaskArtifact {
    pub kind: String,
    pub value: Value,
}

/// Artifact directive keys recognized as the artifact's kind. Any other key in
/// the object (e.g. `target`) is a modifier retained inside [`CaskArtifact::value`].
pub const CASK_ARTIFACT_KINDS: &[&str] = &[
    "app",
    "suite",
    "appimage",
    "binary",
    "manpage",
    "pkg",
    "installer",
    "uninstall",
    "zap",
    "preflight",
    "postflight",
    "uninstall_preflight",
    "uninstall_postflight",
    "font",
    "colorpicker",
    "dictionary",
    "input_method",
    "internet_plugin",
    "keyboard_layout",
    "prefpane",
    "qlplugin",
    "mdimporter",
    "screen_saver",
    "service",
    "audio_unit_plugin",
    "vst_plugin",
    "vst3_plugin",
    "artifact",
    "stage_only",
    "preflight_steps",
    "postflight_steps",
    "uninstall_preflight_steps",
    "uninstall_postflight_steps",
    "zsh_completion",
    "bash_completion",
    "fish_completion",
];

/// A cask `depends_on` block. `cask` / `formula` are lifted for convenience;
/// everything (including `macos`, `arch`, ...) stays in `raw`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CaskDependsOn {
    pub cask: Vec<String>,
    pub formula: Vec<String>,
    pub macos: Option<Value>,
    pub raw: Value,
}

/// Parse a `formula.jws.json` payload into merged, typed formulae.
pub(crate) fn parse_formulae(payload: &[u8], tag: &BottleTag) -> Result<Vec<Formula>, ApiError> {
    let value: Value = serde_json::from_slice(payload).map_err(|source| ApiError::Json {
        context: "formula.jws.json".to_string(),
        source,
    })?;
    let Value::Array(items) = value else {
        return Err(ApiError::invalid(
            "formula.jws.json",
            "expected a JSON array of formula objects",
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(formula_from_value(item, tag)?);
    }
    Ok(out)
}

/// Parse a `cask.jws.json` payload into merged, typed casks.
pub(crate) fn parse_casks(payload: &[u8], tag: &BottleTag) -> Result<Vec<Cask>, ApiError> {
    let value: Value = serde_json::from_slice(payload).map_err(|source| ApiError::Json {
        context: "cask.jws.json".to_string(),
        source,
    })?;
    let Value::Array(items) = value else {
        return Err(ApiError::invalid(
            "cask.jws.json",
            "expected a JSON array of cask objects",
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(cask_from_value(item, tag)?);
    }
    Ok(out)
}

/// Apply `variations[<tag>]` wholesale over the top-level keys, then drop the
/// `variations` key entirely. Missing or non-object variations are ignored.
fn merge_variations(obj: &mut Map<String, Value>, tag: &BottleTag) {
    let Some(Value::Object(variations)) = obj.remove("variations") else {
        return;
    };
    if let Some(Value::Object(overrides)) = variations.get(&tag.to_string()) {
        for (key, value) in overrides {
            obj.insert(key.clone(), value.clone());
        }
    }
}

fn formula_from_value(item: Value, tag: &BottleTag) -> Result<Formula, ApiError> {
    let Value::Object(mut obj) = item else {
        return Err(ApiError::invalid(
            "formula.jws.json",
            "expected each formula entry to be a JSON object",
        ));
    };
    merge_variations(&mut obj, tag);
    let raw = Value::Object(obj.clone());

    let rf: RawFormula =
        serde_json::from_value(Value::Object(obj)).map_err(|source| ApiError::Json {
            context: "formula entry".to_string(),
            source,
        })?;

    let version = match rf.versions.stable.as_deref() {
        Some(s) if !s.is_empty() => s.parse::<Version>().unwrap_or_else(|_| Version::null()),
        _ => Version::null(),
    };
    let pkg_version = PkgVersion::new(version, rf.revision);

    let bottle = rf.bottle.stable.map(|bs| {
        let mut files: Vec<BottleFile> = bs
            .files
            .into_iter()
            .filter_map(|(tag_str, bf)| {
                let tag = tag_str.parse::<BottleTag>().ok()?;
                let sha256 = bf.sha256.parse::<Checksum>().ok()?;
                Some(BottleFile {
                    tag,
                    cellar: bf.cellar,
                    url: bf.url,
                    sha256,
                })
            })
            .collect();
        files.sort_by_key(|a| a.tag.to_string());
        Bottle {
            rebuild: bs.rebuild,
            root_url: bs.root_url,
            files,
        }
    });

    let mut dependencies = Vec::new();
    for name in rf.dependencies {
        dependencies.push(Dependency {
            name,
            tags: Vec::new(),
        });
    }
    for name in rf.build_dependencies {
        dependencies.push(Dependency {
            name,
            tags: vec![DependencyTag::Build],
        });
    }
    for name in rf.test_dependencies {
        dependencies.push(Dependency {
            name,
            tags: vec![DependencyTag::Test],
        });
    }
    for name in rf.recommended_dependencies {
        dependencies.push(Dependency {
            name,
            tags: vec![DependencyTag::Recommended],
        });
    }
    for name in rf.optional_dependencies {
        dependencies.push(Dependency {
            name,
            tags: vec![DependencyTag::Optional],
        });
    }

    let mut uses_from_macos = Vec::new();
    for (i, element) in rf.uses_from_macos.iter().enumerate() {
        if let Some((name, tags)) = parse_uses_from_macos(element) {
            let since = rf
                .uses_from_macos_bounds
                .get(i)
                .and_then(|b| b.since.clone());
            uses_from_macos.push(UsesFromMacos { name, tags, since });
        }
    }

    let conflicts_with = rf
        .conflicts_with
        .into_iter()
        .enumerate()
        .map(|(i, name)| Conflict {
            name,
            reason: rf.conflicts_with_reasons.get(i).cloned().flatten(),
        })
        .collect();

    let keg_only_reason = rf.keg_only_reason.map(|k| KegOnlyReason {
        reason: k.reason.unwrap_or_default(),
        explanation: k.explanation.unwrap_or_default(),
    });

    Ok(Formula {
        name: rf.name,
        full_name: rf.full_name,
        tap: rf.tap,
        oldnames: rf.oldnames,
        aliases: rf.aliases,
        desc: rf.desc,
        license: rf.license,
        homepage: rf.homepage,
        pkg_version,
        revision: rf.revision,
        version_scheme: rf.version_scheme,
        bottle_defined: rf.versions.bottle,
        bottle,
        pour_bottle_only_if: rf.pour_bottle_only_if,
        keg_only: rf.keg_only,
        keg_only_reason,
        dependencies,
        uses_from_macos,
        conflicts_with,
        link_overwrite: rf.link_overwrite,
        caveats: rf.caveats,
        deprecated: rf.deprecated,
        deprecation_reason: rf.deprecation_reason,
        disabled: rf.disabled,
        disable_reason: rf.disable_reason,
        ruby_source_path: rf.ruby_source_path,
        tap_git_head: rf.tap_git_head,
        post_install_defined: rf.post_install_defined,
        service: rf.service,
        raw,
    })
}

/// Parse one `uses_from_macos` element: a bare name string, or a single-key
/// object mapping the name to one tag or a list of tags.
fn parse_uses_from_macos(element: &Value) -> Option<(String, Vec<DependencyTag>)> {
    match element {
        Value::String(name) => Some((name.clone(), Vec::new())),
        Value::Object(map) => {
            let (name, tags_value) = map.iter().next()?;
            let tags = match tags_value {
                Value::String(t) => dependency_tag(t).into_iter().collect(),
                Value::Array(items) => items
                    .iter()
                    .filter_map(|v| v.as_str().and_then(dependency_tag))
                    .collect(),
                _ => Vec::new(),
            };
            Some((name.clone(), tags))
        }
        _ => None,
    }
}

fn dependency_tag(s: &str) -> Option<DependencyTag> {
    match s {
        "build" => Some(DependencyTag::Build),
        "test" => Some(DependencyTag::Test),
        "recommended" => Some(DependencyTag::Recommended),
        "optional" => Some(DependencyTag::Optional),
        _ => None,
    }
}

fn cask_from_value(item: Value, tag: &BottleTag) -> Result<Cask, ApiError> {
    let Value::Object(mut obj) = item else {
        return Err(ApiError::invalid(
            "cask.jws.json",
            "expected each cask entry to be a JSON object",
        ));
    };
    merge_variations(&mut obj, tag);
    let raw = Value::Object(obj.clone());

    let rc: RawCask =
        serde_json::from_value(Value::Object(obj)).map_err(|source| ApiError::Json {
            context: "cask entry".to_string(),
            source,
        })?;

    let artifacts = rc.artifacts.into_iter().map(cask_artifact).collect();
    let depends_on = cask_depends_on(rc.depends_on);

    Ok(Cask {
        token: rc.token,
        old_tokens: rc.old_tokens,
        name: rc.name,
        desc: rc.desc,
        homepage: rc.homepage,
        version: rc.version.filter(|v| !v.is_empty()),
        sha256: rc.sha256,
        url: rc.url,
        artifacts,
        depends_on,
        caveats: rc.caveats,
        auto_updates: rc.auto_updates.unwrap_or(false),
        deprecated: rc.deprecated,
        disabled: rc.disabled,
        disable_reason: rc.disable_reason,
        raw,
    })
}

/// Turn one raw artifact into a [`CaskArtifact`], classifying its kind by the
/// first recognized directive key. Unknown object keys are preserved for
/// fail-closed handling downstream; non-object and empty artifacts receive a
/// stable malformed marker so they cannot disappear during parsing.
fn cask_artifact(value: Value) -> CaskArtifact {
    let kind = value
        .as_object()
        .and_then(|obj| {
            obj.keys()
                .find(|key| CASK_ARTIFACT_KINDS.contains(&key.as_str()))
                .or_else(|| obj.keys().next())
        })
        .cloned()
        .unwrap_or_else(|| "<malformed>".to_owned());
    CaskArtifact { kind, value }
}

fn cask_depends_on(value: Value) -> CaskDependsOn {
    let cask = string_array(&value, "cask");
    let formula = string_array(&value, "formula");
    let macos = value.get("macos").cloned();
    CaskDependsOn {
        cask,
        formula,
        macos,
        raw: value,
    }
}

/// Extract a `key` whose value is a string or an array of strings, as a
/// `Vec<String>` (a bare string becomes a single-element vec).
fn string_array(value: &Value, key: &str) -> Vec<String> {
    match value.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawFormula {
    #[serde(default)]
    name: String,
    #[serde(default)]
    full_name: String,
    #[serde(default)]
    tap: Option<String>,
    #[serde(default)]
    oldnames: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    versions: RawVersions,
    #[serde(default)]
    revision: u32,
    #[serde(default)]
    version_scheme: u32,
    #[serde(default)]
    bottle: RawBottleWrap,
    #[serde(default)]
    pour_bottle_only_if: Option<String>,
    #[serde(default)]
    keg_only: bool,
    #[serde(default)]
    keg_only_reason: Option<RawKegOnlyReason>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    build_dependencies: Vec<String>,
    #[serde(default)]
    test_dependencies: Vec<String>,
    #[serde(default)]
    recommended_dependencies: Vec<String>,
    #[serde(default)]
    optional_dependencies: Vec<String>,
    #[serde(default)]
    uses_from_macos: Vec<Value>,
    #[serde(default)]
    uses_from_macos_bounds: Vec<RawBound>,
    #[serde(default)]
    conflicts_with: Vec<String>,
    #[serde(default)]
    conflicts_with_reasons: Vec<Option<String>>,
    #[serde(default)]
    link_overwrite: Vec<String>,
    #[serde(default)]
    caveats: Option<String>,
    #[serde(default)]
    deprecated: bool,
    #[serde(default)]
    deprecation_reason: Option<String>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    disable_reason: Option<String>,
    #[serde(default)]
    ruby_source_path: Option<String>,
    #[serde(default)]
    tap_git_head: Option<String>,
    #[serde(default)]
    post_install_defined: bool,
    #[serde(default)]
    service: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVersions {
    #[serde(default)]
    stable: Option<String>,
    #[serde(default)]
    bottle: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawBottleWrap {
    #[serde(default)]
    stable: Option<RawBottleStable>,
}

#[derive(Debug, Deserialize)]
struct RawBottleStable {
    #[serde(default)]
    rebuild: u32,
    #[serde(default)]
    root_url: String,
    #[serde(default)]
    files: BTreeMap<String, RawBottleFile>,
}

#[derive(Debug, Deserialize)]
struct RawBottleFile {
    #[serde(default)]
    cellar: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    sha256: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawKegOnlyReason {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    explanation: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawBound {
    #[serde(default)]
    since: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCask {
    #[serde(default)]
    token: String,
    #[serde(default)]
    old_tokens: Vec<String>,
    #[serde(default)]
    name: Vec<String>,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    artifacts: Vec<Value>,
    #[serde(default)]
    depends_on: Value,
    #[serde(default)]
    caveats: Option<String>,
    #[serde(default)]
    auto_updates: Option<bool>,
    #[serde(default)]
    deprecated: bool,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    disable_reason: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use zapbrew_types::Arch;

    fn linux_x86() -> BottleTag {
        BottleTag::Linux { arch: Arch::X86_64 }
    }

    const FULL_FORMULA: &str = r#"[
      {
        "name": "wget",
        "full_name": "wget",
        "tap": "homebrew/core",
        "oldnames": ["wget2"],
        "aliases": ["wgot"],
        "desc": "Internet file retriever",
        "license": "GPL-3.0-or-later",
        "homepage": "https://www.gnu.org/software/wget/",
        "versions": {"stable": "1.25.0", "head": null, "bottle": true},
        "revision": 2,
        "version_scheme": 1,
        "bottle": {
          "stable": {
            "rebuild": 1,
            "root_url": "https://ghcr.io/v2/homebrew/core",
            "files": {
              "arm64_sonoma": {"cellar": ":any", "url": "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:aaaa", "sha256": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"},
              "x86_64_linux": {"cellar": "/home/linuxbrew/.linuxbrew/Cellar", "url": "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:bbbb", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
            }
          }
        },
        "keg_only": true,
        "keg_only_reason": {"reason": ":provided_by_macos", "explanation": "macOS ships wget"},
        "dependencies": ["libidn2", "openssl@3"],
        "build_dependencies": ["pkg-config"],
        "test_dependencies": ["python"],
        "recommended_dependencies": ["gettext"],
        "optional_dependencies": ["libproxy"],
        "uses_from_macos": ["zlib", {"curl": "build"}, {"m4": ["build", "test"]}],
        "uses_from_macos_bounds": [{}, {"since": "catalina"}, {}],
        "conflicts_with": ["wgetx"],
        "conflicts_with_reasons": ["both install a wget binary"],
        "link_overwrite": ["bin/wget"],
        "caveats": "some caveat",
        "deprecated": true,
        "deprecation_reason": "unmaintained",
        "disabled": false,
        "disable_reason": null,
        "ruby_source_path": "Formula/w/wget.rb",
        "tap_git_head": "deadbeef",
        "post_install_defined": true,
        "service": {"run": ["bin/wget", "--daemon"]}
      }
    ]"#;

    #[test]
    fn parses_live_field_variants() {
        let formulae = parse_formulae(FULL_FORMULA.as_bytes(), &linux_x86()).unwrap();
        assert_eq!(formulae.len(), 1);
        let f = &formulae[0];

        assert_eq!(f.name, "wget");
        assert_eq!(f.oldnames, ["wget2"]);
        assert_eq!(f.aliases, ["wgot"]);
        assert_eq!(f.desc.as_deref(), Some("Internet file retriever"));
        assert_eq!(f.license.as_deref(), Some("GPL-3.0-or-later"));
        assert_eq!(f.pkg_version.to_string(), "1.25.0_2");
        assert_eq!(f.revision, 2);
        assert_eq!(f.version_scheme, 1);
        assert!(f.bottle_defined);
        assert!(f.keg_only);
        let reason = f.keg_only_reason.as_ref().unwrap();
        assert_eq!(reason.reason, ":provided_by_macos");
        assert_eq!(reason.explanation, "macOS ships wget");
        assert!(f.deprecated);
        assert_eq!(f.deprecation_reason.as_deref(), Some("unmaintained"));
        assert!(!f.disabled);
        assert_eq!(f.ruby_source_path.as_deref(), Some("Formula/w/wget.rb"));
        assert_eq!(f.tap_git_head.as_deref(), Some("deadbeef"));
        assert!(f.post_install_defined);
        assert!(f.service.is_some());
        assert_eq!(f.link_overwrite, ["bin/wget"]);
        assert_eq!(f.caveats.as_deref(), Some("some caveat"));

        assert_eq!(f.conflicts_with.len(), 1);
        assert_eq!(f.conflicts_with[0].name, "wgetx");
        assert_eq!(
            f.conflicts_with[0].reason.as_deref(),
            Some("both install a wget binary")
        );
    }

    #[test]
    fn converts_bottle_files_keyed_by_tag() {
        let f = &parse_formulae(FULL_FORMULA.as_bytes(), &linux_x86()).unwrap()[0];
        let bottle = f.bottle.as_ref().unwrap();
        assert_eq!(bottle.rebuild, 1);
        assert_eq!(bottle.root_url, "https://ghcr.io/v2/homebrew/core");
        // Sorted deterministically by tag spelling: arm64_sonoma < x86_64_linux.
        assert_eq!(bottle.files.len(), 2);
        assert_eq!(bottle.files[0].tag.to_string(), "arm64_sonoma");
        assert_eq!(bottle.files[1].tag.to_string(), "x86_64_linux");

        let linux = f.bottle_file(&linux_x86()).unwrap();
        assert_eq!(linux.cellar, "/home/linuxbrew/.linuxbrew/Cellar");
        assert_eq!(
            linux.sha256.as_str(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        // Checksum normalizes case at parse.
        let mac = bottle
            .file_for(&BottleTag::MacOs {
                arch: Arch::Arm64,
                version: zapbrew_types::MacOsVersion::Sonoma,
            })
            .unwrap();
        assert_eq!(
            mac.sha256.as_str(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn maps_dependency_tags_and_bounds() {
        let f = &parse_formulae(FULL_FORMULA.as_bytes(), &linux_x86()).unwrap()[0];

        let by_name = |n: &str| f.dependencies.iter().find(|d| d.name == n).unwrap();
        assert!(by_name("libidn2").is_required_runtime());
        assert!(by_name("libidn2").tags.is_empty());
        assert!(by_name("pkg-config").is_build());
        assert!(by_name("python").is_test());
        assert!(by_name("gettext").is_recommended());
        assert!(by_name("libproxy").is_optional());
        // Group order: runtime, build, test, recommended, optional.
        let names: Vec<&str> = f.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "libidn2",
                "openssl@3",
                "pkg-config",
                "python",
                "gettext",
                "libproxy"
            ]
        );

        assert_eq!(f.uses_from_macos.len(), 3);
        let zlib = &f.uses_from_macos[0];
        assert_eq!(zlib.name, "zlib");
        assert!(zlib.tags.is_empty());
        assert_eq!(zlib.since, None);
        let curl = &f.uses_from_macos[1];
        assert_eq!(curl.name, "curl");
        assert_eq!(curl.tags, [DependencyTag::Build]);
        assert_eq!(curl.since.as_deref(), Some("catalina"));
        let m4 = &f.uses_from_macos[2];
        assert_eq!(m4.name, "m4");
        assert_eq!(m4.tags, [DependencyTag::Build, DependencyTag::Test]);
        assert_eq!(m4.since, None);
    }

    #[test]
    fn variation_merge_replaces_keys_wholesale() {
        let payload = r#"[
          {
            "name": "foo",
            "versions": {"stable": "1.0", "bottle": true},
            "dependencies": ["base-dep"],
            "variations": {
              "x86_64_linux": {"dependencies": ["linux-dep"], "keg_only": true},
              "arm64_sonoma": {"dependencies": ["mac-dep"]}
            }
          }
        ]"#;

        let f = &parse_formulae(payload.as_bytes(), &linux_x86()).unwrap()[0];
        // Array replaced wholesale, not appended.
        let names: Vec<&str> = f.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["linux-dep"]);
        assert!(f.keg_only);
        // variations key dropped from the retained raw value.
        assert!(f.raw.get("variations").is_none());
        assert_eq!(
            f.raw
                .get("dependencies")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(1)
        );

        // A non-matching tag leaves the base value and still drops variations.
        let mac = &parse_formulae(
            payload.as_bytes(),
            &BottleTag::MacOs {
                arch: Arch::Arm64,
                version: zapbrew_types::MacOsVersion::Sequoia,
            },
        )
        .unwrap()[0];
        let mac_names: Vec<&str> = mac.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(mac_names, ["base-dep"]);
        assert!(mac.raw.get("variations").is_none());
    }

    #[test]
    fn retains_raw_when_no_variations() {
        let payload =
            r#"[{"name": "bar", "versions": {"stable": "2.0"}, "homepage": "https://bar"}]"#;
        let f = &parse_formulae(payload.as_bytes(), &linux_x86()).unwrap()[0];
        let expected: Value = serde_json::from_str(
            r#"{"name": "bar", "versions": {"stable": "2.0"}, "homepage": "https://bar"}"#,
        )
        .unwrap();
        assert_eq!(f.raw, expected);
    }

    #[test]
    fn skips_unknown_bottle_tags_and_bad_checksums() {
        let payload = r#"[
          {
            "name": "baz",
            "versions": {"stable": "1.0", "bottle": true},
            "bottle": {"stable": {"rebuild": 0, "root_url": "u", "files": {
              "some_future_macos": {"cellar": ":any", "url": "u1", "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"},
              "x86_64_linux": {"cellar": ":any", "url": "u2", "sha256": "not-a-valid-checksum"},
              "arm64_linux": {"cellar": ":any", "url": "u3", "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"}
            }}}
          }
        ]"#;
        let f = &parse_formulae(payload.as_bytes(), &linux_x86()).unwrap()[0];
        let bottle = f.bottle.as_ref().unwrap();
        // Unknown tag dropped, bad-checksum entry dropped, valid one kept.
        assert_eq!(bottle.files.len(), 1);
        assert_eq!(bottle.files[0].tag.to_string(), "arm64_linux");
    }

    #[test]
    fn parses_cask_artifacts_and_depends_on() {
        let payload = r#"[
          {
            "token": "everything",
            "old_tokens": ["every-thing"],
            "name": ["Everything"],
            "desc": "Little bit of everything",
            "homepage": "https://everything.app/",
            "version": "1.2.3",
            "sha256": "c64c05bdc0be845505d6e55e69e696a7f50d40846e76155f0c85d5ff5e7bbb84",
            "url": "https://everything.app/Everything.zip",
            "artifacts": [
              {"uninstall": [{"launchctl": "com.every.thing"}]},
              {"installer": [{"script": {"executable": "install.sh"}}]},
              {"app": ["Everything.app"], "target": "$APPDIR/Everything.app"},
              {"zap": [{"trash": ["~/.everything"]}]}
            ],
            "caveats": "kernel extension required",
            "depends_on": {"cask": ["something"], "formula": ["openssl@3"], "macos": {">=": ["10.15"]}},
            "auto_updates": true,
            "deprecated": false,
            "disabled": true,
            "disable_reason": "is discontinued upstream"
          }
        ]"#;
        let casks = parse_casks(payload.as_bytes(), &linux_x86()).unwrap();
        assert_eq!(casks.len(), 1);
        let c = &casks[0];
        assert_eq!(c.token, "everything");
        assert_eq!(c.old_tokens, ["every-thing"]);
        assert_eq!(c.name, ["Everything"]);
        assert_eq!(c.version.as_deref(), Some("1.2.3"));
        assert!(c.auto_updates);
        assert!(c.disabled);
        assert_eq!(
            c.disable_reason.as_deref(),
            Some("is discontinued upstream")
        );

        let kinds: Vec<&str> = c.artifacts.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["uninstall", "installer", "app", "zap"]);
        // app artifact retains its target modifier.
        let app = &c.artifacts[2];
        assert_eq!(
            app.value.get("target").and_then(Value::as_str),
            Some("$APPDIR/Everything.app")
        );

        assert_eq!(c.depends_on.cask, ["something"]);
        assert_eq!(c.depends_on.formula, ["openssl@3"]);
        assert!(c.depends_on.macos.is_some());
    }

    #[test]
    fn cask_unknown_artifact_kind_is_preserved() {
        let payload = r#"[
          {"token": "mystery", "artifacts": [{"future_kind": ["x"]}]}
        ]"#;
        let c = &parse_casks(payload.as_bytes(), &linux_x86()).unwrap()[0];
        assert_eq!(c.artifacts.len(), 1);
        assert_eq!(c.artifacts[0].kind, "future_kind");
        assert!(!c.auto_updates);
    }

    #[test]
    fn cask_no_check_sha_and_raw_retained() {
        let payload = r#"[
          {"token": "t", "sha256": "no_check", "artifacts": [], "depends_on": {}}
        ]"#;
        let c = &parse_casks(payload.as_bytes(), &linux_x86()).unwrap()[0];
        assert_eq!(c.sha256.as_deref(), Some("no_check"));
        assert!(c.artifacts.is_empty());
        let expected: Value = serde_json::from_str(
            r#"{"token": "t", "sha256": "no_check", "artifacts": [], "depends_on": {}}"#,
        )
        .unwrap();
        assert_eq!(c.raw, expected);
    }

    #[test]
    fn non_array_payload_is_an_error() {
        assert!(parse_formulae(b"{}", &linux_x86()).is_err());
        assert!(parse_casks(b"{}", &linux_x86()).is_err());
    }
}
