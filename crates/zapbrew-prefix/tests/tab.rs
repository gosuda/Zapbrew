use camino::Utf8PathBuf;
use serde_json::Value;
use tempfile::tempdir;
use zapbrew_prefix::{SourceVersions, Tab};

/// Live-brew receipt fixture, captured byte-identical from the host:
/// `/home/linuxbrew/.linuxbrew/Cellar/vim/9.2.0900/INSTALL_RECEIPT.json`
/// on 2026-08-03 (Homebrew `6.0.14-48-g964f319`).
fn fixture_path() -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/INSTALL_RECEIPT.json"),
    )
    .expect("fixture path is valid UTF-8")
}

#[test]
fn fixture_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let tab = Tab::load(fixture_path())?;
    let output = tab.to_pretty_json()?;
    let output_value: Value = serde_json::from_str(output.trim_end())?;
    let fixture_value: Value = serde_json::from_str(&std::fs::read_to_string(fixture_path())?)?;
    assert_eq!(output_value, fixture_value);
    Ok(())
}

#[test]
fn pretty_json_format() -> Result<(), Box<dyn std::error::Error>> {
    let tab = Tab::load(fixture_path())?;
    let json = tab.to_pretty_json()?;
    assert!(json.ends_with('\n'), "must end with a newline");
    assert!(!json.ends_with("\n\n"), "must end with exactly one newline");
    assert!(json.starts_with("{\n  "), "must use 2-space indentation");
    assert!(!json.contains('\t'), "must not contain tabs");
    let _: Value = serde_json::from_str(json.trim_end())?;
    Ok(())
}

#[test]
fn unknown_and_obsolete_fields_dropped() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = Utf8PathBuf::from_path_buf(dir.path().join("receipt.json"))
        .expect("temp path is valid UTF-8");
    let extra = r#"{
  "homebrew_version": "4.0.0",
  "HEAD": "e5b3a1d2c4f6a8b0d2e4f6a8b0d2e4f6a8b0d2e4",
  "alias_path": "/usr/local/Library/...",
  "installed_as_dependency": true,
  "unknown_key": 123
}"#;
    std::fs::write(&path, extra)?;

    let tab = Tab::load(&path)?;
    let output = tab.to_pretty_json()?;
    let output_value: Value = serde_json::from_str(output.trim_end())?;

    assert!(output_value.get("HEAD").is_none());
    assert!(output_value.get("alias_path").is_none());
    assert!(output_value.get("installed_as_dependency").is_none());
    assert!(output_value.get("unknown_key").is_none());
    assert!(tab.installed_on_request);
    Ok(())
}

#[test]
fn stdlib_omitted_when_null() -> Result<(), Box<dyn std::error::Error>> {
    let tab = Tab {
        stdlib: None,
        ..Tab::default()
    };
    let output = tab.to_pretty_json()?;
    assert!(!output.contains("\"stdlib\""));

    let with_stdlib = Tab {
        stdlib: Some("libc++".to_string()),
        ..Tab::default()
    };
    let output = with_stdlib.to_pretty_json()?;
    assert!(output.contains("\"stdlib\": \"libc++\""));
    Ok(())
}

#[test]
fn nested_runtime_source_and_built_on() -> Result<(), Box<dyn std::error::Error>> {
    let tab = Tab::load(fixture_path())?;

    let deps = tab
        .runtime_dependencies
        .as_ref()
        .expect("runtime_dependencies present");
    let dep = deps.first().expect("one dependency");
    assert_eq!(dep.full_name, "libsodium");
    assert_eq!(dep.version, "1.0.22");
    assert_eq!(dep.pkg_version, "1.0.22");
    assert_eq!(dep.revision, 0);
    assert_eq!(dep.bottle_rebuild, Some(0));
    assert_eq!(dep.compatibility_version, None);
    assert!(dep.declared_directly);

    assert_eq!(tab.source.tap.as_deref(), Some("homebrew/core"));
    assert_eq!(tab.source.spec, "stable");
    assert_eq!(
        tab.source.versions,
        SourceVersions {
            stable: Some("9.2.0900".to_string()),
            head: None,
            version_scheme: 0,
            compatibility_version: None,
        }
    );

    let built_on = tab.built_on.as_ref().expect("built_on present");
    assert_eq!(built_on.get("os").and_then(|v| v.as_deref()), Some("Linux"));
    assert_eq!(
        built_on.get("os_version").and_then(|v| v.as_deref()),
        Some("Ubuntu 24.04.4")
    );
    assert_eq!(
        built_on.get("cpu_family").and_then(|v| v.as_deref()),
        Some("zen5")
    );
    assert_eq!(
        built_on.get("glibc_version").and_then(|v| v.as_deref()),
        Some("2.39")
    );
    assert_eq!(
        built_on.get("oldest_cpu_family").and_then(|v| v.as_deref()),
        Some("core2")
    );
    Ok(())
}

#[test]
fn missing_file_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = Utf8PathBuf::from_path_buf(dir.path().join("missing.json"))
        .expect("temp path is valid UTF-8");
    let tab = Tab::load(&path)?;
    assert_eq!(tab, Tab::default());
    Ok(())
}

#[test]
fn corrupt_and_empty_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;

    let corrupt = Utf8PathBuf::from_path_buf(dir.path().join("corrupt.json"))
        .expect("temp path is valid UTF-8");
    std::fs::write(&corrupt, "{ not json")?;
    assert_eq!(Tab::load(&corrupt)?, Tab::default());

    let empty = Utf8PathBuf::from_path_buf(dir.path().join("empty.json"))
        .expect("temp path is valid UTF-8");
    std::fs::write(&empty, "")?;
    assert_eq!(Tab::load(&empty)?, Tab::default());

    Ok(())
}

#[test]
fn invalid_utf8_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("invalid.json");
    std::fs::write(&path, [0xff, 0xfe])?;
    let utf8_path = Utf8PathBuf::from_path_buf(path).expect("temp path is valid UTF-8");
    assert_eq!(Tab::load(utf8_path)?, Tab::default());
    Ok(())
}

#[test]
fn write_creates_parents_and_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = Utf8PathBuf::from_path_buf(dir.path().join("nested/dir/receipt.json"))
        .expect("temp path is valid UTF-8");
    let original = Tab::load(fixture_path())?;
    original.write(&path)?;
    assert!(path.is_file());
    let loaded = Tab::load(&path)?;
    assert_eq!(loaded, original);
    Ok(())
}
