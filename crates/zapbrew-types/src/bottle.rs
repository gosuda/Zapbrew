//! Bottle tags and bottle file records — zero-I/O value types.
//!
//! `BottleTag` models Homebrew's bottle tag strings (`x86_64_linux`,
//! `arm64_linux`, `arm64_<macos_name>`, `<macos_name>` for Intel macOS,
//! `all`). Parsing and formatting are pure; host detection (`BottleTag` for
//! the running machine) lives in `zapbrew-prefix`'s `Env::detect`, which uses
//! [`BottleTag::from_host`] with the OS/arch/version it gathered.

use std::fmt;
use std::str::FromStr;

use crate::Checksum;
use crate::TypeError;

/// CPU architectures that Homebrew bottles are built for.
///
/// Only the two 64-bit architectures the bottle JSON uses are modeled.
/// `Ord` is derived and orders `X86_64 < Arm64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Arch {
    X86_64,
    Arm64,
}

impl Arch {
    /// Canonical tag spelling for this architecture.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Arm64 => "arm64",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Arch {
    type Err = TypeError;

    /// Accepts the two canonical tag spellings only. Non-canonical host
    /// spellings (`aarch64`, …) are handled by [`BottleTag::from_host`], not
    /// here.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "x86_64" => Ok(Self::X86_64),
            "arm64" => Ok(Self::Arm64),
            _ => Err(TypeError::InvalidBottleTag(s.to_string())),
        }
    }
}

/// A macOS release, ordered by the name→number table in
/// `.references/brew/Library/Homebrew/macos_version.rb` (`MacOSVersion::SYMBOLS`).
///
/// Variant order is ascending macOS version, so `Ord` supports older-tag
/// fallback comparison (net's tag selection walks versions descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MacOsVersion {
    /// macOS 10.15
    Catalina,
    /// macOS 11
    BigSur,
    /// macOS 12
    Monterey,
    /// macOS 13
    Ventura,
    /// macOS 14
    Sonoma,
    /// macOS 15
    Sequoia,
    /// macOS 26
    Tahoe,
    /// macOS 27
    GoldenGate,
}

impl MacOsVersion {
    /// Every macOS version in the source table, ascending.
    pub const ALL: [Self; 8] = [
        Self::Catalina,
        Self::BigSur,
        Self::Monterey,
        Self::Ventura,
        Self::Sonoma,
        Self::Sequoia,
        Self::Tahoe,
        Self::GoldenGate,
    ];

    /// The symbol name used in bottle tags (`big_sur`, `golden_gate`, …).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Catalina => "catalina",
            Self::BigSur => "big_sur",
            Self::Monterey => "monterey",
            Self::Ventura => "ventura",
            Self::Sonoma => "sonoma",
            Self::Sequoia => "sequoia",
            Self::Tahoe => "tahoe",
            Self::GoldenGate => "golden_gate",
        }
    }

    /// Maps a numeric macOS product version (`sw_vers -productVersion` output)
    /// to the symbol-table entry it corresponds to.
    ///
    /// Symbol names are rejected here — use [`FromStr`] or
    /// [`BottleTag::from_host`], which tries symbols first. Returns `None` for
    /// malformed input (empty, whitespace, leading/trailing dots, empty
    /// components, more than three dot-separated parts, non-numeric components)
    /// and for versions outside the modeled table (including `10` without
    /// minor `15`, and majors not in `SYMBOLS`).
    pub fn from_product_version(s: &str) -> Option<Self> {
        if s.is_empty() || s.as_bytes().iter().any(|b| b.is_ascii_whitespace()) {
            return None;
        }
        if s.starts_with('.') || s.ends_with('.') {
            return None;
        }

        let mut parts = s.split('.');
        let major_str = parts.next()?;
        if major_str.is_empty() {
            return None;
        }
        let major: u32 = major_str.parse().ok()?;

        if let Some(minor_str) = parts.next() {
            if minor_str.is_empty() {
                return None;
            }
            minor_str.parse::<u32>().ok()?;
        }

        if let Some(patch_str) = parts.next() {
            if patch_str.is_empty() {
                return None;
            }
            patch_str.parse::<u32>().ok()?;
        }

        if parts.next().is_some() {
            return None;
        }

        match major {
            10 => {
                let minor_str = s.split('.').nth(1)?;
                if minor_str.parse::<u32>().ok()? != 15 {
                    return None;
                }
                Some(Self::Catalina)
            }
            11 => Some(Self::BigSur),
            12 => Some(Self::Monterey),
            13 => Some(Self::Ventura),
            14 => Some(Self::Sonoma),
            15 => Some(Self::Sequoia),
            26 => Some(Self::Tahoe),
            27 => Some(Self::GoldenGate),
            _ => None,
        }
    }
}

impl fmt::Display for MacOsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MacOsVersion {
    type Err = TypeError;

    /// Parses a bottle-tag macOS symbol name. Unknown names (including
    /// numeric forms such as `"15"` and disabled names such as `"mojave"`)
    /// are rejected; only the current `SYMBOLS` table is modeled.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "catalina" => Ok(Self::Catalina),
            "big_sur" => Ok(Self::BigSur),
            "monterey" => Ok(Self::Monterey),
            "ventura" => Ok(Self::Ventura),
            "sonoma" => Ok(Self::Sonoma),
            "sequoia" => Ok(Self::Sequoia),
            "tahoe" => Ok(Self::Tahoe),
            "golden_gate" => Ok(Self::GoldenGate),
            _ => Err(TypeError::InvalidMacOsVersion(s.to_string())),
        }
    }
}

/// A Homebrew bottle tag: OS, architecture, and (on macOS) the release.
///
/// Display matches Homebrew's canonical tag spelling
/// (`Utils::Bottles::Tag#to_s`): Linux tags always carry the arch,
/// Intel macOS tags are the bare macOS name, arm64 macOS tags are
/// `arm64_<name>`, and the universal tag is `all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BottleTag {
    Linux { arch: Arch },
    MacOs { arch: Arch, version: MacOsVersion },
    All,
}

impl BottleTag {
    /// Builds the tag for a host from OS, architecture, and (on macOS) the
    /// macOS version — pure, no environment reads or subprocesses.
    ///
    /// Accepts the canonical `"linux"`/`"macos"` OS names plus `"darwin"`
    /// (the Darwin kernel name), and both the canonical arch spellings and
    /// `"aarch64"` (what `std::env::consts::ARCH` yields on arm64 hosts).
    /// The host shape is never [`BottleTag::All`]; any unrecognized input
    /// returns `None`.
    pub fn from_host(os: &str, arch: &str, macos_version: Option<&str>) -> Option<Self> {
        let arch = match arch {
            "x86_64" => Arch::X86_64,
            "arm64" | "aarch64" => Arch::Arm64,
            _ => return None,
        };
        match os {
            "linux" => Some(Self::Linux { arch }),
            "macos" | "darwin" => {
                let raw = macos_version?;
                let version = raw
                    .parse::<MacOsVersion>()
                    .ok()
                    .or_else(|| MacOsVersion::from_product_version(raw))?;
                Some(Self::MacOs { arch, version })
            }
            _ => None,
        }
    }
}

impl fmt::Display for BottleTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => f.write_str("all"),
            Self::Linux { arch } => write!(f, "{arch}_linux"),
            Self::MacOs {
                arch: Arch::X86_64,
                version,
            } => f.write_str(version.as_str()),
            Self::MacOs {
                arch: Arch::Arm64,
                version,
            } => write!(f, "arm64_{version}"),
        }
    }
}

impl FromStr for BottleTag {
    type Err = TypeError;

    /// Parses a canonical bottle tag. Only the tag forms the bottle JSON
    /// emits are accepted: `all`, `x86_64_linux`, `arm64_linux`,
    /// `arm64_<macos_name>`, and `<macos_name>` (bare = Intel macOS).
    ///
    /// Unknown arch prefixes, unknown OS/system names, unknown macOS names,
    /// and structurally malformed tags all fail with
    /// [`TypeError::InvalidBottleTag`] embedding the whole offending tag.
    /// The non-canonical `x86_64_<macos_name>` form is rejected too (Intel
    /// macOS tags are always the bare name), and non-canonical arch
    /// spellings such as `aarch64_linux` are rejected — tag data is
    /// canonical; host-detection spellings go through [`BottleTag::from_host`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            return Ok(Self::All);
        }
        if let Some(system) = s.strip_prefix("x86_64_") {
            // `x86_64_linux` is the only canonical `x86_64_` tag; an Intel
            // macOS tag would be the bare name, so anything else is invalid.
            if system == "linux" {
                Ok(Self::Linux { arch: Arch::X86_64 })
            } else {
                Err(TypeError::InvalidBottleTag(s.to_string()))
            }
        } else if let Some(system) = s.strip_prefix("arm64_") {
            if system == "linux" {
                Ok(Self::Linux { arch: Arch::Arm64 })
            } else if let Ok(version) = system.parse::<MacOsVersion>() {
                Ok(Self::MacOs {
                    arch: Arch::Arm64,
                    version,
                })
            } else {
                Err(TypeError::InvalidBottleTag(s.to_string()))
            }
        } else {
            // Bare tag: Intel macOS when the name is known, else invalid.
            if let Ok(version) = s.parse::<MacOsVersion>() {
                Ok(Self::MacOs {
                    arch: Arch::X86_64,
                    version,
                })
            } else {
                Err(TypeError::InvalidBottleTag(s.to_string()))
            }
        }
    }
}

/// A single bottle file entry as it appears in the formula JSON's
/// `bottle.stable.files.<tag>` map. Constructed by `zapbrew-api` and
/// consumed by `zapbrew-net`; lives here so both siblings stay independent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BottleFile {
    pub tag: BottleTag,
    pub cellar: String,
    pub url: String,
    pub sha256: Checksum,
}
