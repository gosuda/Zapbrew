//! Integration tests for `BottleTag`, `Arch`, `MacOsVersion`, and `BottleFile`.
//!
//! The macOS symbol table and its order come verbatim from
//! `.references/brew/Library/Homebrew/macos_version.rb` (`MacOSVersion::SYMBOLS`):
//! golden_gate 27, tahoe 26, sequoia 15, sonoma 14, ventura 13, monterey 12,
//! big_sur 11, catalina 10.15.
//!
//! `MacOsVersion::from_product_version` maps numeric product versions
//! (`sw_vers -productVersion` output) to that same table: `10.15[.patch]` →
//! catalina, and majors `11`…`15`, `26`, `27` by major with patches ignored.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use zapbrew_types::{Arch, BottleFile, BottleTag, Checksum, MacOsVersion, TypeError};

/// The name→version table from `macos_version.rb`, ascending.
const SYMBOLS: [(&str, MacOsVersion); 8] = [
    ("catalina", MacOsVersion::Catalina),
    ("big_sur", MacOsVersion::BigSur),
    ("monterey", MacOsVersion::Monterey),
    ("ventura", MacOsVersion::Ventura),
    ("sonoma", MacOsVersion::Sonoma),
    ("sequoia", MacOsVersion::Sequoia),
    ("tahoe", MacOsVersion::Tahoe),
    ("golden_gate", MacOsVersion::GoldenGate),
];

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn checksum(hex: &str) -> Checksum {
    hex.parse()
        .expect("64 lowercase hex chars is a valid checksum")
}

fn full_hex(digit: char) -> String {
    std::iter::repeat_n(digit, 64).collect()
}

// ---------------------------------------------------------------------------
// MacOsVersion: every symbol in the source table.
// ---------------------------------------------------------------------------

#[test]
fn macos_version_round_trips_every_source_symbol() {
    for (name, version) in SYMBOLS {
        assert_eq!(
            name.parse::<MacOsVersion>().expect("known symbol"),
            version,
            "{name}"
        );
        assert_eq!(version.as_str(), name, "{name}");
        assert_eq!(version.to_string(), name, "{name}");
        assert_eq!(
            version
                .as_str()
                .parse::<MacOsVersion>()
                .expect("display round-trips"),
            version,
            "{name}"
        );
    }
}

#[test]
fn macos_version_all_matches_the_source_table() {
    assert_eq!(MacOsVersion::ALL.len(), SYMBOLS.len());
    for ((name, version), all) in SYMBOLS.iter().zip(MacOsVersion::ALL) {
        assert_eq!(*version, all, "{name}");
    }
}

#[test]
fn macos_version_total_order_matches_version_numbers() {
    // Order by the numeric macOS version: 10.15 < 11 < 12 < 13 < 14 < 15 < 26 < 27.
    for pair in SYMBOLS.windows(2) {
        assert!(
            pair[0].1 < pair[1].1,
            "{} should sort below {}",
            pair[0].0,
            pair[1].0
        );
        assert!(
            pair[1].1 > pair[0].1,
            "{} should sort above {}",
            pair[1].0,
            pair[0].0
        );
    }
    // Every symbol is distinct and equals itself.
    let unique: HashSet<MacOsVersion> = SYMBOLS.iter().map(|(_, v)| *v).collect();
    assert_eq!(unique.len(), SYMBOLS.len());
    for (name, version) in SYMBOLS {
        assert_eq!(version, version, "{name}");
    }
}

#[test]
fn macos_version_rejects_unknown_names() {
    for bad in [
        "mojave",
        "high_sierra",
        "sierra",
        "el_capitan",
        "dunno",
        "15",
        "10.15",
        "27",
        "Sequoia",
        "sequoia ",
        "big sur",
        "",
    ] {
        let err = bad
            .parse::<MacOsVersion>()
            .expect_err("unknown name must be rejected");
        assert!(
            matches!(&err, TypeError::InvalidMacOsVersion(_)),
            "wrong variant for {bad:?}: {err}"
        );
        assert!(
            err.to_string().contains(bad),
            "error for {bad:?} must embed it: {err}"
        );
    }
    // Numeric forms are the product-version boundary, not the symbol
    // boundary: they stay rejected here and are handled by
    // `MacOsVersion::from_product_version` / `BottleTag::from_host` instead.
}

// ---------------------------------------------------------------------------
// MacOsVersion::from_product_version: `sw_vers -productVersion` mapping.
// ---------------------------------------------------------------------------

#[test]
fn macos_version_parses_product_versions() {
    // Real `sw_vers -productVersion` output, including patch components.
    for (product, expected) in [
        ("10.15", MacOsVersion::Catalina),
        ("10.15.7", MacOsVersion::Catalina),
        ("11", MacOsVersion::BigSur),
        ("11.0", MacOsVersion::BigSur),
        ("11.7.10", MacOsVersion::BigSur),
        ("12.6.8", MacOsVersion::Monterey),
        ("13.6", MacOsVersion::Ventura),
        ("14.5", MacOsVersion::Sonoma),
        ("15.6", MacOsVersion::Sequoia),
        ("15.6.1", MacOsVersion::Sequoia),
        ("26.0", MacOsVersion::Tahoe),
        ("27.1.2", MacOsVersion::GoldenGate),
    ] {
        assert_eq!(
            MacOsVersion::from_product_version(product),
            Some(expected),
            "{product}"
        );
    }
}

#[test]
fn macos_version_rejects_malformed_product_versions() {
    for bad in [
        "",         // empty
        "10",       // Catalina needs its 10.15 minor
        "10.14",    // 10.x minor other than 15 is not in the table
        "10.16",    // 10.16 is not a shipped release in the table
        "10.15.",   // trailing dot: empty patch
        "11.",      // trailing dot: empty minor
        ".11",      // leading dot: empty major
        "11.0.1.2", // more than major.minor.patch
        "abc",      // non-numeric
        "11.x",     // non-numeric minor
        "10.15.x",  // non-numeric patch
        "1.15",     // major 1 is not in the symbol table
        "9", "0", "-1", " 11", // whitespace is not trimmed
        "11 ", "sonoma", // symbols go through FromStr, not the product parser
    ] {
        assert_eq!(
            MacOsVersion::from_product_version(bad),
            None,
            "{bad:?} must be rejected"
        );
    }
}

#[test]
fn macos_version_rejects_unsupported_product_versions() {
    // 16 and 28 are gaps above the table; 25 sits below tahoe 26.
    for bad in ["16", "16.0", "16.3.1", "25", "25.0.1", "28", "28.0"] {
        assert_eq!(
            MacOsVersion::from_product_version(bad),
            None,
            "{bad:?} must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// Arch.
// ---------------------------------------------------------------------------

#[test]
fn arch_parses_and_displays_canonical_spellings() {
    assert_eq!("x86_64".parse::<Arch>().expect("x86_64"), Arch::X86_64);
    assert_eq!("arm64".parse::<Arch>().expect("arm64"), Arch::Arm64);
    assert_eq!(Arch::X86_64.to_string(), "x86_64");
    assert_eq!(Arch::Arm64.to_string(), "arm64");
    assert_eq!(Arch::X86_64.as_str(), "x86_64");
    assert_eq!(Arch::Arm64.as_str(), "arm64");
}

#[test]
fn arch_rejects_unknown_spellings() {
    // Non-canonical spellings are host-detection inputs, not tag data.
    for bad in ["riscv64", "aarch64", "amd64", "x86", "arm", "i386", ""] {
        let err = bad
            .parse::<Arch>()
            .expect_err("unknown arch must be rejected");
        assert!(
            matches!(&err, TypeError::InvalidBottleTag(_)),
            "wrong variant for {bad:?}"
        );
        assert!(
            err.to_string().contains(bad),
            "error for {bad:?} must embed it"
        );
    }
}

#[test]
fn arch_total_order() {
    assert!(Arch::X86_64 < Arch::Arm64);
    assert!(Arch::Arm64 > Arch::X86_64);
}

// ---------------------------------------------------------------------------
// BottleTag: canonical forms round-trip.
// ---------------------------------------------------------------------------

fn canonical_tag_strings() -> Vec<String> {
    let mut tags = vec![
        "all".to_string(),
        "x86_64_linux".to_string(),
        "arm64_linux".to_string(),
    ];
    for (name, _) in SYMBOLS {
        tags.push(format!("arm64_{name}"));
        tags.push(name.to_string());
    }
    tags
}

#[test]
fn all_tag_forms_round_trip() {
    let tags = canonical_tag_strings();
    for s in &tags {
        let tag = s.parse::<BottleTag>().expect("canonical tag parses");
        assert_eq!(tag.to_string(), *s, "display must round-trip {s:?}");
    }
    // The five documented forms parse to the right shapes.
    assert_eq!("all".parse::<BottleTag>().expect("all"), BottleTag::All);
    assert_eq!(
        "x86_64_linux".parse::<BottleTag>().expect("x86_64_linux"),
        BottleTag::Linux { arch: Arch::X86_64 }
    );
    assert_eq!(
        "arm64_linux".parse::<BottleTag>().expect("arm64_linux"),
        BottleTag::Linux { arch: Arch::Arm64 }
    );
    // Bare name = Intel macOS; prefixed name = arm64 macOS.
    for (name, version) in SYMBOLS {
        let bare = name.parse::<BottleTag>().expect("bare macos tag");
        assert_eq!(
            bare,
            BottleTag::MacOs {
                arch: Arch::X86_64,
                version
            },
            "{name}"
        );
        let prefixed = format!("arm64_{name}")
            .parse::<BottleTag>()
            .expect("arm64 macos tag");
        assert_eq!(
            prefixed,
            BottleTag::MacOs {
                arch: Arch::Arm64,
                version
            },
            "arm64_{name}"
        );
    }
}

#[test]
fn all_tag_forms_are_distinct() {
    let mut seen: HashSet<BottleTag> = HashSet::new();
    for s in canonical_tag_strings() {
        let tag = s.parse::<BottleTag>().expect("canonical tag parses");
        assert!(seen.insert(tag), "duplicate tag parsed from {s:?}");
    }
    assert_eq!(seen.len(), canonical_tag_strings().len());
}

#[test]
fn malformed_tags_are_rejected_through_type_error() {
    let bad = [
        "",
        "linux",         // bare Linux is never emitted; Linux tags always carry the arch
        "windows",       // unknown OS
        "mojave",        // unknown macOS name
        "high_sierra",   // disabled, not in the symbol table
        "dunno",         // brew's fallback symbol, not a real version
        "x86_64",        // arch without a system
        "arm64",         // arch without a system
        "_linux",        // empty arch prefix
        "x86_64_",       // empty system
        "arm64_",        // empty system
        "riscv64_linux", // unknown arch prefix
        "arm_linux",     // non-canonical arch spelling
        "aarch64_linux", // host-detection spelling, not canonical tag data
        "i386_linux",    // non-canonical arch spelling
        "ppc64_linux",   // non-canonical arch spelling
        "x86_64_sonoma", // non-canonical: Intel macOS tags are always bare
        "all_arm64",     // `all` never carries an arch
        "all_linux",
        "arm64_windows", // unknown OS after a valid arch prefix
        "arm64_mojave",  // unknown macOS name after a valid arch prefix
        "arm64_sonoma_extra",
        "x86_64_linux_extra",
        "sonoma_x86_64", // arch after the system, never emitted
    ];
    for s in bad {
        let err = s
            .parse::<BottleTag>()
            .expect_err("malformed tag must be rejected");
        assert!(
            matches!(&err, TypeError::InvalidBottleTag(_)),
            "wrong variant for {s:?}: {err}"
        );
        assert!(
            err.to_string().contains(s),
            "error for {s:?} must embed it: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// BottleTag::from_host: pure host mapping (symbols and product versions).
// ---------------------------------------------------------------------------

#[test]
fn from_host_maps_linux_hosts() {
    assert_eq!(
        BottleTag::from_host("linux", "x86_64", None),
        Some(BottleTag::Linux { arch: Arch::X86_64 })
    );
    assert_eq!(
        BottleTag::from_host("linux", "arm64", None),
        Some(BottleTag::Linux { arch: Arch::Arm64 })
    );
    // `std::env::consts::ARCH` reports `aarch64` on arm64 hosts.
    assert_eq!(
        BottleTag::from_host("linux", "aarch64", None),
        Some(BottleTag::Linux { arch: Arch::Arm64 })
    );
    // The macOS version is ignored on Linux.
    assert_eq!(
        BottleTag::from_host("linux", "x86_64", Some("sonoma")),
        Some(BottleTag::Linux { arch: Arch::X86_64 })
    );
}

#[test]
fn from_host_maps_macos_hosts() {
    for (name, version) in SYMBOLS {
        assert_eq!(
            BottleTag::from_host("macos", "x86_64", Some(name)),
            Some(BottleTag::MacOs {
                arch: Arch::X86_64,
                version
            }),
            "{name}"
        );
        assert_eq!(
            BottleTag::from_host("macos", "arm64", Some(name)),
            Some(BottleTag::MacOs {
                arch: Arch::Arm64,
                version
            }),
            "{name}"
        );
        assert_eq!(
            BottleTag::from_host("darwin", "aarch64", Some(name)),
            Some(BottleTag::MacOs {
                arch: Arch::Arm64,
                version
            }),
            "darwin/{name}"
        );
    }
}

#[test]
fn from_host_rejects_unrecognized_shapes() {
    // Unknown OS.
    assert_eq!(BottleTag::from_host("windows", "x86_64", None), None);
    assert_eq!(BottleTag::from_host("freebsd", "x86_64", None), None);
    assert_eq!(BottleTag::from_host("", "x86_64", None), None);
    // Unknown arch.
    assert_eq!(BottleTag::from_host("linux", "riscv64", None), None);
    assert_eq!(BottleTag::from_host("linux", "arm", None), None);
    assert_eq!(BottleTag::from_host("macos", "x86", Some("sonoma")), None);
    // macOS requires a parseable version.
    assert_eq!(BottleTag::from_host("macos", "x86_64", None), None);
    assert_eq!(BottleTag::from_host("macos", "x86_64", Some("")), None);
    assert_eq!(
        BottleTag::from_host("macos", "x86_64", Some("mojave")),
        None
    );
    assert_eq!(BottleTag::from_host("macos", "x86_64", Some("99")), None);
    // Malformed and unsupported numeric product versions are rejected too.
    assert_eq!(BottleTag::from_host("macos", "x86_64", Some("10")), None);
    assert_eq!(BottleTag::from_host("macos", "x86_64", Some("10.14")), None);
    assert_eq!(BottleTag::from_host("macos", "x86_64", Some("16")), None);
    assert_eq!(BottleTag::from_host("macos", "x86_64", Some("25")), None);
    assert_eq!(BottleTag::from_host("macos", "x86_64", Some("28")), None);
    assert_eq!(
        BottleTag::from_host("darwin", "aarch64", Some("16.0")),
        None
    );
}

#[test]
fn from_host_maps_numeric_macos_product_versions() {
    // Real `sw_vers -productVersion` output is numeric; from_host must map
    // it on both Intel and arm64 hosts.
    for (product, version) in [
        ("10.15", MacOsVersion::Catalina),
        ("10.15.7", MacOsVersion::Catalina),
        ("11.0", MacOsVersion::BigSur),
        ("15.6", MacOsVersion::Sequoia),
        ("26.0", MacOsVersion::Tahoe),
        ("27.1.2", MacOsVersion::GoldenGate),
    ] {
        assert_eq!(
            BottleTag::from_host("macos", "x86_64", Some(product)),
            Some(BottleTag::MacOs {
                arch: Arch::X86_64,
                version
            }),
            "{product}"
        );
        assert_eq!(
            BottleTag::from_host("darwin", "aarch64", Some(product)),
            Some(BottleTag::MacOs {
                arch: Arch::Arm64,
                version
            }),
            "darwin/{product}"
        );
    }
}

// ---------------------------------------------------------------------------
// BottleFile: equality and hash.
// ---------------------------------------------------------------------------

fn sample_bottle_file() -> BottleFile {
    BottleFile {
        tag: "arm64_sonoma".parse().expect("valid tag"),
        cellar: ":any".to_string(),
        url: "https://ghcr.io/v2/homebrew/core/wget/blobs/sha256:0000".to_string(),
        sha256: checksum(&full_hex('a')),
    }
}

#[test]
fn bottle_file_equality_and_hash() {
    let a = sample_bottle_file();
    let b = sample_bottle_file();
    assert_eq!(a, b, "identical fields are equal");
    assert_eq!(hash_of(&a), hash_of(&b), "equal values hash equal");

    // Each field change breaks equality.
    let mut changed = a.clone();
    changed.tag = "sonoma".parse().expect("valid tag");
    assert_ne!(a, changed, "tag participates in equality");

    changed = a.clone();
    changed.cellar = ":any_skip_relocation".to_string();
    assert_ne!(a, changed, "cellar participates in equality");

    changed = a.clone();
    changed.url = "https://example.com/other".to_string();
    assert_ne!(a, changed, "url participates in equality");

    changed = a.clone();
    changed.sha256 = checksum(&full_hex('b'));
    assert_ne!(a, changed, "sha256 participates in equality");
}

#[test]
fn bottle_file_hash_agrees_with_equality_in_a_set() {
    let a = sample_bottle_file();
    let b = sample_bottle_file();
    let mut set = HashSet::new();
    set.insert(a.clone());
    set.insert(b); // equal value: no duplicate entry
    assert_eq!(set.len(), 1, "equal BottleFiles dedupe in a HashSet");
    set.insert(BottleFile {
        tag: "arm64_linux".parse().expect("valid tag"),
        ..a
    });
    assert_eq!(set.len(), 2, "different tags stay distinct");
}
