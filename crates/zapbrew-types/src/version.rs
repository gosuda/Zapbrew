//! Homebrew-compatible [`Version`] / [`PkgVersion`] total order.
//!
//! Tokenization follows `.references/brew/Library/Homebrew/version.rb`
//! (`SCAN_PATTERN` + Appendix A). Comparison uses one canonical token sequence
//! shared by [`Ord`], [`PartialEq`], and [`Hash`]: numeric zeros are dropped
//! right-to-left only when the next retained token is absent or nonnumeric
//! (kept when the next retained token is numeric), so interstitial zeros stay
//! significant while trailing zeros before a nonnumeric/end still collapse.
//! Sequence exhaustion is padded with an explicit End rank. After scan, a
//! terminal uppercase [`Token::String`] `"B"` immediately following a numeric
//! token is normalized to [`Token::Beta`] with empty revision (Beta(0)) so
//! Erlang forms like `R14B` order under the fixed Beta rank without treating
//! bare lowercase `a` as a prerelease.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;

use crate::TypeError;

/// Formula / resource version with Homebrew's token total order.
#[derive(Debug, Clone)]
pub struct Version {
    /// Original input string. Empty when null.
    raw: String,
    tokens: Vec<Token>,
    is_head: bool,
    /// Set when constructed via [`Version::detect`] / [`Version::detect_with_tag`].
    detected_from_url: bool,
}

/// Version plus bottle/formula revision (`version` or `version_N`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PkgVersion {
    pub version: Version,
    pub revision: u32,
}
#[derive(Debug, Clone)]
enum Token {
    /// Alphabetic token; value is the matched lexeme.
    String(String),
    /// Digit run; value is the matched digit string (arbitrary length).
    Numeric(String),
    /// Composite tokens store only the numeric revision (may be empty = 0).
    Alpha {
        rev: String,
    },
    Beta {
        rev: String,
    },
    Pre {
        rev: String,
    },
    Rc {
        rev: String,
    },
    Patch {
        rev: String,
    },
    Post {
        rev: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CanonToken {
    String(String),
    /// Canonical numeric digits with leading zeros stripped. Interstitial
    /// numeric zeros are retained as empty digit strings (value zero).
    Numeric(String),
    Alpha(String),
    Beta(String),
    Pre(String),
    Rc(String),
    Patch(String),
    Post(String),
}

static HEAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^HEAD(?:-.*)?$").expect("HEAD regex"));

/// Leftmost-first union matching Homebrew `SCAN_PATTERN` priority:
/// Alpha, Beta, Pre, RC, Patch, Post, Numeric, String.
static SCAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)",
        r"(?:",
        r"alpha[0-9]*|a[0-9]+",
        r"|beta[0-9]*|b[0-9]+",
        r"|pre[0-9]*",
        r"|rc[0-9]*",
        r"|p[0-9]*",
        r"|\.post[0-9]+",
        r"|[0-9]+",
        r"|[a-z]+",
        r")",
    ))
    .expect("SCAN_PATTERN regex")
});

static ALPHA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\A(?:alpha[0-9]*|a[0-9]+)\z").expect("alpha"));
static BETA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\A(?:beta[0-9]*|b[0-9]+)\z").expect("beta"));
static PRE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\Apre[0-9]*\z").expect("pre"));
static RC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\Arc[0-9]*\z").expect("rc"));
static PATCH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\Ap[0-9]*\z").expect("patch"));
static POST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\A\.post[0-9]+\z").expect("post"));
static NUMERIC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9]+\z").expect("numeric"));

impl Version {
    /// Null version (empty string). Least element; equal only to itself.
    #[must_use]
    pub fn null() -> Self {
        Self {
            raw: String::new(),
            tokens: Vec::new(),
            is_head: false,
            detected_from_url: false,
        }
    }

    /// Whether this is the null version.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.raw.is_empty()
    }

    /// Whether this matches `^HEAD(?:-.*)?$`.
    #[must_use]
    pub fn is_head(&self) -> bool {
        self.is_head
    }

    /// Original version string (empty for null).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Whether this version was produced by URL/stem detection.
    #[must_use]
    pub fn detected_from_url(&self) -> bool {
        self.detected_from_url
    }

    /// Detect a version from a download URL using Homebrew's 26-parser priority list.
    ///
    /// See `.references/brew/Library/Homebrew/version.rb` `VERSION_PARSERS` and Appendix A.
    #[must_use]
    pub fn detect(url: &str) -> Self {
        Self::parse_from_spec(url, true)
    }

    /// Like [`Self::detect`], but prefer an explicit VCS `tag` over the URL path
    /// (`Version.detect(url, tag: ...)` in Ruby).
    #[must_use]
    pub fn detect_with_tag(_url: &str, tag: &str) -> Self {
        Self::parse_from_spec(tag, true)
    }

    fn parse_tokens(s: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut idx = 0;
        while idx < s.len() {
            let Some(m) = SCAN_RE.find_at(s, idx) else {
                break;
            };
            tokens.push(Token::from_match(m.as_str()));
            idx = m.end();
        }
        tokens
    }

    /// Canonical token sequence shared by [`Ord`], [`PartialEq`] and [`Hash`].
    ///
    /// 1. Scan tokens unchanged (`SCAN_PATTERN`).
    /// 2. Source-driven normalization: a terminal uppercase `String("B")`
    ///    immediately after a [`Token::Numeric`] becomes `Beta` with empty
    ///    revision (`Beta(0)`). Bare lowercase `a` / `b` stay strings.
    /// 3. Right-to-left zero canonicalization: drop `Numeric(0)` only when the
    ///    next retained token is absent or nonnumeric; keep it when the next
    ///    retained token is numeric. So `0.1 == 0.1.0`, `2.1.0-p194 == 2.1-p194`,
    ///    while `1.0.2 != 1.2` and `0.1 != 1`.
    /// 4. Remaining tokens map to [`CanonToken`]: specials/Patch/Post store
    ///    leading-zero-stripped revisions; numerics store leading-zero-stripped
    ///    digits (empty = zero); strings store the raw matched lexeme.
    fn canonical_tokens(&self) -> Vec<CanonToken> {
        let tokens = Self::normalize_tokens(&self.tokens);
        let mut retained_rev: Vec<CanonToken> = Vec::with_capacity(tokens.len());
        // `None` = no retained token to the right yet; `Some(true)` = numeric;
        // `Some(false)` = nonnumeric.
        let mut next_retained_is_numeric: Option<bool> = None;
        for t in tokens.iter().rev() {
            if let Token::Numeric(n) = t
                && is_zero_digits(n)
            {
                // Keep only when the next retained token is numeric.
                if next_retained_is_numeric == Some(true) {
                    retained_rev.push(t.canonical());
                    next_retained_is_numeric = Some(true);
                }
                continue;
            }
            let c = t.canonical();
            let is_numeric = matches!(c, CanonToken::Numeric(_));
            retained_rev.push(c);
            next_retained_is_numeric = Some(is_numeric);
        }
        retained_rev.reverse();
        retained_rev
    }

    /// Erlang / OTP forms like `R14B` scan as `String("B")` (bare `B` is not
    /// `b[0-9]+`). Homebrew's lexeme fallthrough ordered that below `R14B01`
    /// (`Beta`). Under fixed ranks, map a terminal uppercase `"B"` after a
    /// numeric token to `Beta(0)` so `R14B < R14B01` without promoting bare
    /// lowercase `a` into Alpha.
    fn normalize_tokens(tokens: &[Token]) -> Vec<Token> {
        let mut out = Vec::with_capacity(tokens.len());
        for t in tokens {
            match t {
                Token::String(s) if s == "B" && matches!(out.last(), Some(Token::Numeric(_))) => {
                    out.push(Token::Beta { rev: String::new() });
                }
                other => out.push(other.clone()),
            }
        }
        out
    }

    /// Compare two canonical sequences, padding exhaustion with an explicit End
    /// rank so the result is symmetric and total.
    ///
    /// Class order: Alpha < Beta < Pre < RC < End < Post < String < Patch <
    /// positive Numeric. Within a class, specials/Patch/Post/Numeric compare by
    /// [`cmp_digit_strings`] and strings by raw `str` ordering; across classes the
    /// fixed rank decides. End has no value, so it only ever ties another End
    /// (both sequences exhausted at the same index).
    fn cmp_canonical(left: &[CanonToken], right: &[CanonToken]) -> Ordering {
        let max = left.len().max(right.len());
        for i in 0..max {
            let ord = match (left.get(i), right.get(i)) {
                (None, None) => Ordering::Equal,
                (None, Some(r)) => END_RANK.cmp(&canon_rank(r)),
                (Some(l), None) => canon_rank(l).cmp(&END_RANK),
                (Some(l), Some(r)) => cmp_canon_token(l, r),
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    }
}

impl FromStr for Version {
    type Err = TypeError;

    /// Never rejects input. Empty string becomes the null version.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

impl Version {
    fn parse(s: &str) -> Self {
        Self::parse_with_flag(s, false)
    }

    fn parse_with_flag(s: &str, detected_from_url: bool) -> Self {
        if s.is_empty() {
            return Self::null();
        }
        let is_head = HEAD_RE.is_match(s);
        let tokens = Self::parse_tokens(s);
        Self {
            raw: s.to_owned(),
            tokens,
            is_head,
            detected_from_url,
        }
    }

    /// Ruby `Version.parse(spec, detected_from_url:)`.
    fn parse_from_spec(spec: &str, detected_from_url: bool) -> Self {
        let decoded = if detected_from_url {
            decode_www_form_component(spec)
        } else {
            spec.to_owned()
        };
        let url_text = decoded.as_str();
        let stem = stem_for_spec(url_text);
        for parser in version_parsers() {
            let haystack = match parser.kind {
                SpecKind::Url => url_text,
                SpecKind::Stem => stem.as_str(),
            };
            if let Some(mut version) = parser.extract(haystack) {
                if parser.underscore_to_dot {
                    version = version.replace('_', ".");
                }
                if !version.is_empty() {
                    return Self::parse_with_flag(&version, detected_from_url);
                }
            }
        }
        Self::null()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.is_null() && other.is_null() {
            return Ordering::Equal;
        }
        if self.is_null() {
            return Ordering::Less;
        }
        if other.is_null() {
            return Ordering::Greater;
        }
        if self.raw == other.raw {
            return Ordering::Equal;
        }
        if self.is_head && other.is_head {
            return Ordering::Equal;
        }
        if self.is_head {
            return Ordering::Greater;
        }
        if other.is_head {
            return Ordering::Less;
        }
        Self::cmp_canonical(&self.canonical_tokens(), &other.canonical_tokens())
    }
}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if self.is_null() {
            0u8.hash(state);
            return;
        }
        if self.is_head {
            1u8.hash(state);
            return;
        }
        2u8.hash(state);
        self.canonical_tokens().hash(state);
    }
}

impl Token {
    fn from_match(val: &str) -> Self {
        let rev = || extract_rev(val);

        if ALPHA_RE.is_match(val) {
            Self::Alpha { rev: rev() }
        } else if BETA_RE.is_match(val) {
            Self::Beta { rev: rev() }
        } else if RC_RE.is_match(val) {
            Self::Rc { rev: rev() }
        } else if PRE_RE.is_match(val) {
            Self::Pre { rev: rev() }
        } else if PATCH_RE.is_match(val) {
            Self::Patch { rev: rev() }
        } else if POST_RE.is_match(val) {
            Self::Post { rev: rev() }
        } else if NUMERIC_RE.is_match(val) {
            Self::Numeric(val.to_owned())
        } else {
            Self::String(val.to_owned())
        }
    }

    fn canonical(&self) -> CanonToken {
        match self {
            Self::String(s) => CanonToken::String(s.clone()),
            Self::Numeric(n) => CanonToken::Numeric(strip_leading_zeros(n)),
            Self::Alpha { rev } => CanonToken::Alpha(strip_leading_zeros(rev)),
            Self::Beta { rev } => CanonToken::Beta(strip_leading_zeros(rev)),
            Self::Pre { rev } => CanonToken::Pre(strip_leading_zeros(rev)),
            Self::Rc { rev } => CanonToken::Rc(strip_leading_zeros(rev)),
            Self::Patch { rev } => CanonToken::Patch(strip_leading_zeros(rev)),
            Self::Post { rev } => CanonToken::Post(strip_leading_zeros(rev)),
        }
    }
}

fn extract_rev(val: &str) -> String {
    val.chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect()
}

fn is_zero_digits(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b == b'0')
}

fn strip_leading_zeros(n: &str) -> String {
    let stripped = n.trim_start_matches('0');
    if stripped.is_empty() {
        String::new()
    } else {
        stripped.to_owned()
    }
}

fn cmp_digit_strings(a: &str, b: &str) -> Ordering {
    let a_n = strip_leading_zeros(a);
    let b_n = strip_leading_zeros(b);
    // empty means zero
    match (a_n.is_empty(), b_n.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a_n.len().cmp(&b_n.len()).then_with(|| a_n.cmp(&b_n)),
    }
}

/// Fixed total class rank: Alpha < Beta < Pre < RC < End < Post < String < Patch
/// < positive Numeric. End sits between RC and Post so a sequence that ends
/// (End) outranks one that continues with a pre-release class but is outranked
/// by one that continues with Post or higher.
const END_RANK: u8 = 4;

fn canon_rank(t: &CanonToken) -> u8 {
    match t {
        CanonToken::Alpha(_) => 0,
        CanonToken::Beta(_) => 1,
        CanonToken::Pre(_) => 2,
        CanonToken::Rc(_) => 3,
        CanonToken::Post(_) => 5,
        CanonToken::String(_) => 6,
        CanonToken::Patch(_) => 7,
        CanonToken::Numeric(_) => 8,
    }
}

fn cmp_canon_token(a: &CanonToken, b: &CanonToken) -> Ordering {
    let ra = canon_rank(a);
    let rb = canon_rank(b);
    if ra != rb {
        return ra.cmp(&rb);
    }
    // Same class.
    match (a, b) {
        (CanonToken::String(x), CanonToken::String(y)) => x.cmp(y),
        (CanonToken::Alpha(x), CanonToken::Alpha(y))
        | (CanonToken::Beta(x), CanonToken::Beta(y))
        | (CanonToken::Pre(x), CanonToken::Pre(y))
        | (CanonToken::Rc(x), CanonToken::Rc(y))
        | (CanonToken::Patch(x), CanonToken::Patch(y))
        | (CanonToken::Post(x), CanonToken::Post(y))
        | (CanonToken::Numeric(x), CanonToken::Numeric(y)) => cmp_digit_strings(x, y),
        _ => Ordering::Equal,
    }
}

impl PkgVersion {
    #[must_use]
    pub fn new(version: Version, revision: u32) -> Self {
        Self { version, revision }
    }
}

impl FromStr for PkgVersion {
    type Err = TypeError;

    /// Split the last `_` + digits suffix.
    /// Fails on empty input (plan regex requires `.+?`) and revision overflow.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(TypeError::InvalidPkgVersion(s.to_owned()));
        }
        if let Some((prefix, rev_str)) = split_revision(s) {
            let revision: u32 = rev_str
                .parse()
                .map_err(|_| TypeError::InvalidPkgVersion(s.to_owned()))?;
            Ok(Self {
                version: Version::parse(prefix),
                revision,
            })
        } else {
            Ok(Self {
                version: Version::parse(s),
                revision: 0,
            })
        }
    }
}

/// LAST `_<digits>` suffix with non-empty prefix (Homebrew `/\A(.+?)(?:_(\d+))?\z/`).
fn split_revision(s: &str) -> Option<(&str, &str)> {
    let (prefix, suffix) = s.rsplit_once('_')?;
    if prefix.is_empty() {
        return None;
    }
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((prefix, suffix))
}

impl fmt::Display for PkgVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.revision > 0 {
            write!(f, "{}_{}", self.version, self.revision)
        } else {
            write!(f, "{}", self.version)
        }
    }
}

// ---------------------------------------------------------------------------
// URL / stem version detection (version.rb VERSION_PARSERS, Appendix A L237)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum SpecKind {
    Url,
    Stem,
}

struct VersionParser {
    kind: SpecKind,
    regex: &'static Regex,
    underscore_to_dot: bool,
}

impl VersionParser {
    fn extract(&self, haystack: &str) -> Option<String> {
        let caps = self.regex.captures(haystack)?;
        let v = caps.get(1)?.as_str();
        if v.is_empty() {
            None
        } else {
            Some(v.to_owned())
        }
    }
}

fn version_parsers() -> &'static [VersionParser] {
    &VERSION_PARSERS
}

macro_rules! parser_regex {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new($pat).unwrap_or_else(|e| panic!("{}: {e}", stringify!($name)))
        });
    };
}

// 26 parsers in VERSION_PARSERS order (version.rb:375-507).
parser_regex!(P01_DATE, r"(?:^|[._-]?)v?(\d{4}-\d{2}-\d{2})");
parser_regex!(
    P02_GITHUB_TARBALL,
    r"github\.com/.+/(?:zip|tar)ball/(?:v|\w+-)?((?:\d+[._-])+\d*)$"
);
parser_regex!(
    P03_GITHUB_RELEASE,
    concat!(
        r"github\.com/.+/releases/download/(?:[rvV]_?)?(",
        r"(?:\d+(?:\.\d+)+)",
        r")/"
    )
);
parser_regex!(P04_ERLANG, r"[_-]([Rr]\d+[AaBb]\d*(?:-\d+)?)");
parser_regex!(P05_BOOST, r"((?:\d+_)+\d+)$");
parser_regex!(
    P06_NUMERIC_DASH_PATCH,
    concat!(
        r"[_-](",
        r"(?:\d+(?:\.\d+)+)",
        r"-(?:p|P|rc|RC)?\d+)",
        r"(?:[._-](?i:bin|dist|stable|src|sources?|final|full))",
        r"?$"
    )
);
parser_regex!(
    P07_HYPHENATED,
    concat!(
        r"^v?(",
        r"(?:\d+(?:\.\d+)+)",
        r"(?:-",
        r"(?:\d+(?:\.\d+)*)",
        r")+)"
    )
);
parser_regex!(P08_NO_EXT, concat!(r"[-v](", r"(?:\d+(?:\.\d+)*)", r")$"));
parser_regex!(P09_LAME, r"-(\d+-\d+)");
parser_regex!(
    P10_DASH_NUMERIC,
    concat!(r"-(", r"(?:\d+(?:\.\d+)*)", r")$")
);
parser_regex!(
    P11_POST,
    concat!(r"-(", r"(?:\d+(?:\.\d+)*)", r"(.post\d+)?)$")
);
parser_regex!(
    P12_LETTER_RC,
    concat!(r"-(", r"(?:\d+(?:\.\d+)*)", r"(?:[abc]|rc|RC)\d*)$")
);
parser_regex!(
    P13_ALPHA_BETA_RC,
    concat!(r"-(", r"(?:\d+(?:\.\d+)*)", r"-(?:alpha|beta|rc)\d*)$")
);
parser_regex!(
    P14_WIN,
    concat!(r"-(", r"(?:\d+(?:\.\d+){1,2})", r")-w(?:in)?(?:32|64)$")
);
parser_regex!(
    P15_OPAM,
    concat!(r"\.(", r"(?:\d+(?:\.\d+){1,2})", r")\+opam$")
);
parser_regex!(
    P16_ARCH,
    concat!(
        r"[_-](",
        r"(?:\d+(?:\.\d+){1,2})",
        r"(?:-\d+)?)[._-](?:i[36]86|x86|x64(?:[_-](?:32|64))?)$"
    )
);
parser_regex!(
    P17_PRERELEASE,
    concat!(
        r"[-.vV]?(",
        r"(?:\d+(?:\.\d+)+)",
        r"(?:[._-]?(?i:alpha|beta|pre|rc)\.?\d{0,2})",
        r")"
    )
);
parser_regex!(
    P18_TRAILING_NUMERIC,
    concat!(r"(", r"(?:\d+(?:\.\d+)*)", r")$")
);
parser_regex!(
    P19_CONTENT_SUFFIX,
    concat!(
        r"[-vV](",
        r"(?:\d+(?:\.\d+)+)",
        r"[abc]?)",
        r"(?:[._-](?i:bin|dist|stable|src|sources?|final|full))",
        r"$"
    )
);
parser_regex!(P20_DASH_STYLE, concat!(r"-(", r"(?:\d+(?:\.\d+)+)", r")-"));
parser_regex!(
    P21_DEBIAN,
    concat!(r"_(", r"(?:\d+(?:\.\d+)*)", r"[abc]?)\.orig$")
);
parser_regex!(P22_DASH_V, r"-v?(\d[^-]+)");
parser_regex!(P23_UNDERSCORE_V, r"_v?(\d[^_]+)");
parser_regex!(P24_PATH_SEMVER, r"/(?:[rvV]_?)?(\d+\.\d+(?:\.\d+){0,2})");
parser_regex!(P25_JPEG, r"\.v(\d+[a-z]?)");
parser_regex!(
    P26_FALLBACK,
    concat!(
        r"[-.vV]?(",
        r"(?:\d+(?:\.\d+)+)",
        r"(?:[._-]?(?i:alpha|beta|pre|rc)\.?\d{0,2})?",
        r")"
    )
);

static VERSION_PARSERS: LazyLock<Vec<VersionParser>> = LazyLock::new(|| {
    vec![
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P01_DATE,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Url,
            regex: &P02_GITHUB_TARBALL,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Url,
            regex: &P03_GITHUB_RELEASE,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Url,
            regex: &P04_ERLANG,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P05_BOOST,
            underscore_to_dot: true,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P06_NUMERIC_DASH_PATCH,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P07_HYPHENATED,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Url,
            regex: &P08_NO_EXT,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P09_LAME,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P10_DASH_NUMERIC,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P11_POST,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P12_LETTER_RC,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P13_ALPHA_BETA_RC,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P14_WIN,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P15_OPAM,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P16_ARCH,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P17_PRERELEASE,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P18_TRAILING_NUMERIC,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P19_CONTENT_SUFFIX,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P20_DASH_STYLE,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P21_DEBIAN,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P22_DASH_V,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P23_UNDERSCORE_V,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Url,
            regex: &P24_PATH_SEMVER,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Stem,
            regex: &P25_JPEG,
            underscore_to_dot: false,
        },
        VersionParser {
            kind: SpecKind::Url,
            regex: &P26_FALLBACK,
            underscore_to_dot: false,
        },
    ]
});

static SOURCEFORGE_DOWNLOAD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:sourceforge\.net|sf\.net)/.*/download$").expect("sourceforge")
});
static NO_FILE_EXTENSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.[^a-zA-Z]+$").expect("no-ext"));
static BOTTLE_EXTNAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.([a-z0-9_]+)\.bottle\.(?:(\d+)\.)?tar\.gz$").expect("bottle ext")
});
static ARCHIVE_EXTNAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\.(tar|cpio|pax)\.(gz|bz2|lz|xz|zst|Z))\z").expect("archive ext")
});
static VERSIONISH_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d+\.\d+[^.]*\z").expect("versionish"));

/// Homebrew `Pathname#stem` / `StemParser.process_spec`.
fn stem_for_spec(spec: &str) -> String {
    if SOURCEFORGE_DOWNLOAD_RE.is_match(spec) {
        return path_stem(path_dirname(spec));
    }
    if NO_FILE_EXTENSION_RE.is_match(spec) {
        return path_basename(spec).to_owned();
    }
    path_stem(spec)
}

fn path_basename(spec: &str) -> &str {
    let s = spec.trim_end_matches('/');
    s.rsplit('/').next().unwrap_or(s)
}

fn path_dirname(spec: &str) -> &str {
    let s = spec.trim_end_matches('/');
    match s.rfind('/') {
        Some(i) if i > 0 => &s[..i],
        Some(_) => "/",
        None => ".",
    }
}

fn path_stem(spec: &str) -> String {
    let base = path_basename(spec);
    let ext = path_extname(base);
    if ext.is_empty() {
        base.to_owned()
    } else if let Some(stripped) = base.strip_suffix(ext) {
        stripped.to_owned()
    } else {
        base.to_owned()
    }
}

/// Homebrew `Pathname#extname` (bottle / double-archive aware).
fn path_extname(basename: &str) -> &str {
    if let Some(m) = BOTTLE_EXTNAME_RE.find(basename) {
        return m.as_str();
    }
    if let Some(caps) = ARCHIVE_EXTNAME_RE.captures(basename) {
        return caps.get(1).map(|m| m.as_str()).unwrap_or("");
    }
    // Don't treat version numbers as extname.
    if VERSIONISH_EXT_RE.is_match(basename) && !basename.ends_with(".7z") {
        return "";
    }
    match basename.rfind('.') {
        Some(i) if i > 0 => &basename[i..],
        _ => "",
    }
}

/// Rough `URI.decode_www_form_component` for URL version detection.
fn decode_www_form_component(input: &str) -> String {
    let plus_to_space: String = input
        .chars()
        .map(|c| if c == '+' { ' ' } else { c })
        .collect();
    let bytes = plus_to_space.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let h1 = from_hex(bytes[i + 1]);
                let h2 = from_hex(bytes[i + 2]);
                if let (Some(a), Some(b)) = (h1, h2) {
                    out.push((a << 4) | b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod parser_count_tests {
    use super::*;

    #[test]
    fn version_parsers_are_exactly_26() {
        assert_eq!(version_parsers().len(), 26);
        assert_eq!(VERSION_PARSERS.len(), 26);
    }
}
