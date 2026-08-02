//! Integration tests for the value types owned here: `Checksum`,
//! `FormulaName`, and the shared `TypeError`.

use std::collections::HashSet;
use std::str::FromStr;

use zapbrew_types::{Checksum, FormulaName, TypeError};

const SHA256_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn parse_checksum(s: &str) -> Checksum {
    match Checksum::from_str(s) {
        Ok(c) => c,
        Err(e) => panic!("expected {s:?} to parse as a checksum, got {e}"),
    }
}

fn assert_checksum_error(s: &str) -> TypeError {
    match Checksum::from_str(s) {
        Err(e) => e,
        Ok(_) => panic!("expected {s:?} to be rejected as a checksum"),
    }
}

fn parse_name(s: &str) -> FormulaName {
    match FormulaName::from_str(s) {
        Ok(n) => n,
        Err(e) => panic!("expected {s:?} to parse as a formula name, got {e}"),
    }
}

fn assert_name_error(s: &str) -> TypeError {
    match FormulaName::from_str(s) {
        Err(e) => e,
        Ok(_) => panic!("expected {s:?} to be rejected as a formula name"),
    }
}

// ---- Checksum ----

#[test]
fn checksum_accepts_64_hex() {
    let c = parse_checksum(SHA256_EMPTY);
    assert_eq!(c.to_string(), SHA256_EMPTY);
    assert_eq!(c.as_str(), SHA256_EMPTY);
    assert_eq!(c.as_ref(), SHA256_EMPTY);
    assert_eq!(parse_checksum(SHA256_ABC).to_string(), SHA256_ABC);
}

#[test]
fn checksum_rejects_wrong_length() {
    let bad = [
        "",
        "abc",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85", // 63 chars
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b8555", // 65 chars
    ];
    for s in bad {
        assert!(Checksum::from_str(s).is_err(), "{s:?} should be rejected");
    }
}

#[test]
fn checksum_rejects_nonhex() {
    let bad = [
        "z".repeat(64),
        format!("{}g", "0".repeat(63)),
        "é".repeat(64),
        "not-a-checksum-at-all-just-text".to_owned(),
    ];
    for s in &bad {
        let err = assert_checksum_error(s.as_str());
        assert!(
            matches!(&err, TypeError::InvalidChecksum(x) if x.as_str() == s.as_str()),
            "for {s:?}"
        );
    }
}

#[test]
fn checksum_normalizes_uppercase() {
    let c = parse_checksum(&SHA256_EMPTY.to_ascii_uppercase());
    assert_eq!(c.to_string(), SHA256_EMPTY);
    assert_eq!(c.as_ref(), SHA256_EMPTY);
}

#[test]
fn checksum_equality_is_case_insensitive() {
    let lower = parse_checksum(SHA256_EMPTY);
    let upper = parse_checksum(&SHA256_EMPTY.to_ascii_uppercase());
    assert_eq!(lower, upper);

    let mut seen = HashSet::new();
    seen.insert(lower);
    assert!(seen.contains(&upper));
    assert!(!seen.contains(&parse_checksum(SHA256_ABC)));
}

#[test]
fn checksum_error_embeds_offending_input() {
    let bad = "not-a-checksum-at-all-just-text";
    let err = assert_checksum_error(bad);
    assert!(matches!(&err, TypeError::InvalidChecksum(x) if x.as_str() == bad));
}

// ---- FormulaName ----

#[test]
fn formula_name_plain() {
    let n = parse_name("wget");
    assert_eq!(n.name(), "wget");
    assert_eq!(n.tap(), None);
    assert_eq!(n.as_str(), "wget");
    assert_eq!(n.to_string(), "wget");
    assert_eq!(n.as_ref(), "wget");
}

#[test]
fn formula_name_legal_punctuation() {
    for s in [
        "libstdc++",
        "pkg-config",
        "openssl@3",
        "sqlite++",
        "x.y.z",
        "foo_bar",
        "a1_b+c.d@e-f",
        "1password",
        "@0",
        "..",
    ] {
        let n = parse_name(s);
        assert_eq!(n.name(), s, "name() for {s:?}");
        assert_eq!(n.tap(), None);
        assert_eq!(n.as_str(), s);
    }
}

#[test]
fn formula_name_qualified() {
    let n = parse_name("homebrew/core/wget");
    assert_eq!(n.name(), "wget");
    assert_eq!(n.tap(), Some(("homebrew", "core")));
    assert_eq!(n.as_str(), "homebrew/core/wget");

    let n = parse_name("homebrew/core/openssl@3");
    assert_eq!(n.name(), "openssl@3");
    assert_eq!(n.tap(), Some(("homebrew", "core")));
}

#[test]
fn formula_name_qualified_arbitrary_tap_chars() {
    // HOMEBREW_TAP_FORMULA_REGEX allows any non-slash characters in
    // user/repository; the name segment still obeys the name charset.
    let n = parse_name("Some.User+Name/repo-1/wget");
    assert_eq!(n.name(), "wget");
    assert_eq!(n.tap(), Some(("Some.User+Name", "repo-1")));

    // Multibyte user segment: offsets must land on UTF-8 boundaries.
    let n = parse_name("ünïcode/repo/wget");
    assert_eq!(n.name(), "wget");
    assert_eq!(n.tap(), Some(("ünïcode", "repo")));
}

#[test]
fn formula_name_rejects_invalid_chars() {
    for s in [
        "wget!",
        "wget?",
        "wget ",
        "wget$",
        "wget*",
        "wgeté",
        "wget\t",
        "\twget",
        "user/repo/wget!",
    ] {
        let err = assert_name_error(s);
        assert!(
            matches!(&err, TypeError::InvalidFormulaName(x) if x.as_str() == s),
            "for {s:?}"
        );
    }
}

#[test]
fn formula_name_rejects_empty_and_empty_segments() {
    for s in [
        "",
        "/",
        "//",
        "user//name",
        "/repo/name",
        "user/repo/",
        "user/",
        "/name",
    ] {
        let err = assert_name_error(s);
        assert!(
            matches!(&err, TypeError::InvalidFormulaName(x) if x.as_str() == s),
            "for {s:?}"
        );
    }
}

#[test]
fn formula_name_rejects_extra_segments() {
    // Two segments is not a supported shape (tap qualification is exactly
    // three), and more than three segments is overqualified.
    for s in [
        "user/name",
        "homebrew/core",
        "a/b/c/d",
        "a/b/c/d/e",
        "a//b/c",
    ] {
        let err = assert_name_error(s);
        assert!(
            matches!(&err, TypeError::InvalidFormulaName(x) if x.as_str() == s),
            "for {s:?}"
        );
    }
}

#[test]
fn formula_name_error_embeds_offending_input() {
    for s in ["", "wget!", "a/b/c/d", "user//name"] {
        let err = assert_name_error(s);
        assert!(
            matches!(&err, TypeError::InvalidFormulaName(x) if x.as_str() == s),
            "for {s:?}"
        );
    }
}

#[test]
fn formula_name_equality() {
    assert_eq!(parse_name("wget"), parse_name("wget"));
    assert_ne!(parse_name("wget"), parse_name("wget2"));
    assert_ne!(parse_name("wget"), parse_name("homebrew/core/wget"));
    assert_eq!(
        parse_name("homebrew/core/wget"),
        parse_name("homebrew/core/wget")
    );
}
