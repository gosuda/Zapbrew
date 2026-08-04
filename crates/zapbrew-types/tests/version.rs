//! Version / PkgVersion oracle tests transcribed from Homebrew.
//!
//! Primary source: `.references/brew/Library/Homebrew/test/version_spec.rb`.
//! PkgVersion rows: Appendix A floor + `test/pkg_version_spec.rb` comparisons.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::str::FromStr;

use zapbrew_types::{PkgVersion, TypeError, Version};

#[derive(Debug, Clone, Copy)]
enum Comp {
    Eq,
    Lt,
    Gt,
}

fn v(s: &str) -> Version {
    match Version::from_str(s) {
        Ok(v) => v,
        Err(_) => unreachable!("Version::from_str never fails"),
    }
}

fn pv(s: &str) -> PkgVersion {
    match PkgVersion::from_str(s) {
        Ok(v) => v,
        Err(e) => panic!("PkgVersion parse failed for {s:?}: {e}"),
    }
}

fn assert_comp(left: &str, comp: Comp, right: &str) {
    let l = v(left);
    let r = v(right);
    match comp {
        Comp::Eq => {
            assert_eq!(l, r, "{left:?} == {right:?}");
            assert_eq!(l.cmp(&r), Ordering::Equal);
        }
        Comp::Lt => {
            assert!(l < r, "{left:?} < {right:?} (got {:?})", l.cmp(&r));
        }
        Comp::Gt => {
            assert!(l > r, "{left:?} > {right:?} (got {:?})", l.cmp(&r));
        }
    }
}

/// Every Version↔Version comparison assertion from `version_spec.rb`.
///
/// Transcribed oracle row count: 77 (every unique Version↔Version triple in
/// `version_spec.rb`, including the `create("2")`/`create("p194")` null rows at
/// lines 20/25 and 21/26 and the `null_version` rows at lines 80-81).
#[test]
fn version_spec_comparison_oracle() {
    // (left, rel, right) — empty string denotes Version::null()
    let rows: &[(&str, Comp, &str)] = &[
        ("", Comp::Lt, "1"),                        // version_spec.rb:80
        ("", Comp::Lt, "0"),                        // version_spec.rb:81
        ("2", Comp::Gt, ""),                        // version_spec.rb:20,25
        ("p194", Comp::Gt, ""),                     // version_spec.rb:21,26
        ("0.1", Comp::Eq, "0.1.0"),                 // version_spec.rb:130
        ("0.1", Comp::Lt, "0.2"),                   // version_spec.rb:131
        ("1.2.3", Comp::Gt, "1.2.2"),               // version_spec.rb:132
        ("1.2.4", Comp::Lt, "1.2.4.1"),             // version_spec.rb:133
        ("1.2.3", Comp::Gt, "1.2.3alpha4"),         // version_spec.rb:135
        ("1.2.3", Comp::Gt, "1.2.3beta2"),          // version_spec.rb:136
        ("1.2.3", Comp::Gt, "1.2.3rc3"),            // version_spec.rb:137
        ("1.2.3", Comp::Lt, "1.2.3-p34"),           // version_spec.rb:138
        ("HEAD", Comp::Gt, "1.2.3"),                // version_spec.rb:162
        ("HEAD-abcdef", Comp::Gt, "1.2.3"),         // version_spec.rb:163
        ("1.2.3", Comp::Lt, "HEAD"),                // version_spec.rb:164
        ("1.2.3", Comp::Lt, "HEAD-fedcba"),         // version_spec.rb:165
        ("HEAD-abcdef", Comp::Eq, "HEAD-fedcba"),   // version_spec.rb:166
        ("HEAD", Comp::Eq, "HEAD-fedcba"),          // version_spec.rb:167
        ("1.2.3alpha", Comp::Lt, "1.2.3"),          // version_spec.rb:171
        ("1.2.3", Comp::Lt, "1.2.3a"),              // version_spec.rb:172
        ("1.2.3alpha4", Comp::Eq, "1.2.3a4"),       // version_spec.rb:173
        ("1.2.3alpha4", Comp::Eq, "1.2.3A4"),       // version_spec.rb:174
        ("1.2.3alpha4", Comp::Gt, "1.2.3alpha3"),   // version_spec.rb:175
        ("1.2.3alpha4", Comp::Lt, "1.2.3alpha5"),   // version_spec.rb:176
        ("1.2.3alpha4", Comp::Lt, "1.2.3alpha10"),  // version_spec.rb:177
        ("1.2.3alpha4", Comp::Lt, "1.2.3beta2"),    // version_spec.rb:179
        ("1.2.3alpha4", Comp::Lt, "1.2.3rc3"),      // version_spec.rb:180
        ("1.2.3alpha4", Comp::Lt, "1.2.3"),         // version_spec.rb:181
        ("1.2.3alpha4", Comp::Lt, "1.2.3-p34"),     // version_spec.rb:182
        ("1.2.3beta2", Comp::Eq, "1.2.3b2"),        // version_spec.rb:186
        ("1.2.3beta2", Comp::Eq, "1.2.3B2"),        // version_spec.rb:187
        ("1.2.3beta2", Comp::Gt, "1.2.3beta1"),     // version_spec.rb:188
        ("1.2.3beta2", Comp::Lt, "1.2.3beta3"),     // version_spec.rb:189
        ("1.2.3beta2", Comp::Lt, "1.2.3beta10"),    // version_spec.rb:190
        ("1.2.3beta2", Comp::Gt, "1.2.3alpha4"),    // version_spec.rb:192
        ("1.2.3beta2", Comp::Lt, "1.2.3rc3"),       // version_spec.rb:193
        ("1.2.3beta2", Comp::Lt, "1.2.3"),          // version_spec.rb:194
        ("1.2.3beta2", Comp::Lt, "1.2.3-p34"),      // version_spec.rb:195
        ("1.2.3pre9", Comp::Eq, "1.2.3PRE9"),       // version_spec.rb:199
        ("1.2.3pre9", Comp::Gt, "1.2.3pre8"),       // version_spec.rb:200
        ("1.2.3pre8", Comp::Lt, "1.2.3pre9"),       // version_spec.rb:201
        ("1.2.3pre9", Comp::Lt, "1.2.3pre10"),      // version_spec.rb:202
        ("1.2.3pre3", Comp::Gt, "1.2.3alpha2"),     // version_spec.rb:204
        ("1.2.3pre3", Comp::Gt, "1.2.3alpha4"),     // version_spec.rb:205
        ("1.2.3pre3", Comp::Gt, "1.2.3beta3"),      // version_spec.rb:206
        ("1.2.3pre3", Comp::Gt, "1.2.3beta5"),      // version_spec.rb:207
        ("1.2.3pre3", Comp::Lt, "1.2.3rc2"),        // version_spec.rb:208
        ("1.2.3pre3", Comp::Lt, "1.2.3"),           // version_spec.rb:209
        ("1.2.3pre3", Comp::Lt, "1.2.3-p2"),        // version_spec.rb:210
        ("1.2.3rc3", Comp::Eq, "1.2.3RC3"),         // version_spec.rb:214
        ("1.2.3rc3", Comp::Gt, "1.2.3rc2"),         // version_spec.rb:215
        ("1.2.3rc3", Comp::Lt, "1.2.3rc4"),         // version_spec.rb:216
        ("1.2.3rc3", Comp::Lt, "1.2.3rc10"),        // version_spec.rb:217
        ("1.2.3rc3", Comp::Gt, "1.2.3alpha4"),      // version_spec.rb:219
        ("1.2.3rc3", Comp::Gt, "1.2.3beta2"),       // version_spec.rb:220
        ("1.2.3rc3", Comp::Lt, "1.2.3"),            // version_spec.rb:221
        ("1.2.3rc3", Comp::Lt, "1.2.3-p34"),        // version_spec.rb:222
        ("1.2.3-p34", Comp::Eq, "1.2.3-P34"),       // version_spec.rb:226
        ("1.2.3-p34", Comp::Gt, "1.2.3-p33"),       // version_spec.rb:227
        ("1.2.3-p34", Comp::Lt, "1.2.3-p35"),       // version_spec.rb:228
        ("1.2.3-p34", Comp::Gt, "1.2.3-p9"),        // version_spec.rb:229
        ("1.2.3-p34", Comp::Gt, "1.2.3alpha4"),     // version_spec.rb:231
        ("1.2.3-p34", Comp::Gt, "1.2.3beta2"),      // version_spec.rb:232
        ("1.2.3-p34", Comp::Gt, "1.2.3rc3"),        // version_spec.rb:233
        ("1.2.3-p34", Comp::Gt, "1.2.3"),           // version_spec.rb:234
        ("1.2.3.post34", Comp::Gt, "1.2.3.post33"), // version_spec.rb:238
        ("1.2.3.post34", Comp::Lt, "1.2.3.post35"), // version_spec.rb:239
        ("1.2.3.post34", Comp::Gt, "1.2.3rc35"),    // version_spec.rb:241
        ("1.2.3.post34", Comp::Gt, "1.2.3alpha35"), // version_spec.rb:242
        ("1.2.3.post34", Comp::Gt, "1.2.3beta35"),  // version_spec.rb:243
        ("1.2.3.post34", Comp::Gt, "1.2.3"),        // version_spec.rb:244
        ("2.1.0-p194", Comp::Lt, "2.1-p195"),       // version_spec.rb:248
        ("2.1-p195", Comp::Gt, "2.1.0-p194"),       // version_spec.rb:249
        ("2.1-p194", Comp::Lt, "2.1.0-p195"),       // version_spec.rb:250
        ("2.1.0-p195", Comp::Gt, "2.1-p194"),       // version_spec.rb:251
        ("2-p194", Comp::Lt, "2.1-p195"),           // version_spec.rb:252
        ("2.1.0-p194", Comp::Gt, ""),               // version_spec.rb:260
    ];
    assert_eq!(rows.len(), 77, "oracle row count drift");
    for (left, comp, right) in rows {
        assert_comp(left, *comp, right);
    }
}

#[test]
fn version_from_str_never_rejects() {
    for s in [
        "",
        "1.2.3",
        "not a version!!!",
        "HEAD",
        "HEADLESS",
        "HEAD2",
        "$$$",
    ] {
        assert!(Version::from_str(s).is_ok(), "must accept {s:?}");
    }
}

#[test]
fn null_constructor_and_display() {
    let n = Version::null();
    assert!(n.is_null());
    assert_eq!(n.as_str(), "");
    assert_eq!(n.to_string(), "");
    assert_eq!(v(""), n);
    assert!(n < v("0"));
    assert!(n < v("1"));
    assert_eq!(n, Version::null());
}

#[test]
fn head_anchored_regex_only() {
    assert!(v("HEAD").is_head());
    assert!(v("HEAD-abcdef").is_head());
    assert!(v("HEAD-").is_head());
    assert!(!v("HEADLESS").is_head());
    assert!(!v("HEAD2").is_head());
    assert!(!v("XHEAD").is_head());
    assert!(!v("head").is_head());

    // HEADLESS / HEAD2 behave as ordinary versions (not equal to HEAD, not above all)
    assert_ne!(v("HEADLESS"), v("HEAD"));
    assert_ne!(v("HEAD2"), v("HEAD"));
    assert!(v("HEADLESS") < v("HEAD"));
    assert!(v("HEAD2") < v("HEAD"));
}

#[test]
fn head_variants_equal_and_hash_equal() {
    let versions = [
        v("HEAD"),
        v("HEAD-abcdef"),
        v("HEAD-fedcba"),
        v("HEAD-ffffff"),
    ];
    for a in &versions {
        for b in &versions {
            assert_eq!(a, b);
            assert_eq!(a.cmp(b), Ordering::Equal);
        }
    }
    let mut set = HashSet::new();
    for ver in versions {
        set.insert(ver);
    }
    assert_eq!(set.len(), 1);
}

#[test]
fn hash_agrees_with_ord_equality() {
    // Canonical equivalences required by contracts.md
    assert_eq!(v("0.1"), v("0.1.0"));
    assert_eq!(v("1.2.3alpha4"), v("1.2.3a4"));

    let mut set = HashSet::new();
    set.insert(v("0.1"));
    assert!(set.contains(&v("0.1.0")));

    set.insert(v("1.2.3alpha4"));
    assert!(set.contains(&v("1.2.3a4")));
    assert!(set.contains(&v("1.2.3A4")));

    // Unequal versions must not collide in the set for these fixtures
    set.insert(v("0.1.1"));
    assert_eq!(set.len(), 3);
}

#[test]
fn display_preserves_original_string() {
    for s in ["1.2.3", "1.2.3alpha4", "HEAD-abcdef", "2.1.0-p194"] {
        assert_eq!(v(s).to_string(), s);
        assert_eq!(v(s).as_str(), s);
    }
}

#[test]
fn erlang_version_sort_order() {
    // Exact ascending list from version_spec.rb:283-286:
    //   %w[R16B ... R13B02-1].reverse
    // Bare `R14B` / `R16B` normalize terminal uppercase String("B") after a
    // numeric to Beta(0); `R14B01` is Beta(1) via `b[0-9]+`, so R14B < R14B01.
    let ascending = [
        "R13B02-1", "R13B03", "R13B04", "R14B", "R14B01", "R14B02", "R14B03", "R14B04", "R15B01",
        "R15B02", "R15B03", "R15B03-1", "R16B",
    ];
    let mut sorted: Vec<Version> = ascending.iter().map(|s| v(s)).collect();
    sorted.sort();
    let got: Vec<String> = sorted.iter().map(|x| x.to_string()).collect();
    assert_eq!(got, ascending);
    assert!(v("R14B") < v("R14B01"), "Beta(0) < Beta(1): R14B < R14B01");
    assert!(v("R16B") > v("R15B03-1"));
}

/// PkgVersion comparison assertions (Appendix A floor + pkg_version_spec.rb).
#[test]
fn pkg_version_comparison_oracle() {
    // Appendix A floor
    assert!(pv("1.1") > pv("1.0_1"));
    assert!(pv("1.0_1") < pv("1.0_2"));
    assert_eq!(pv("1.0_0"), pv("1.0"));

    // pkg_version_spec.rb
    assert_eq!(pv("1.0_1"), pv("1.0_1"));
    assert_ne!(pv("1.0_1"), pv("1.0_2"));
    assert!(pv("HEAD") > pv("1.0"));
    assert!(pv("1.0_1") < pv("2.0_1"));
    assert!(pv("1.0") < pv("HEAD"));
}

#[test]
fn pkg_version_display_and_parse() {
    assert_eq!(pv("1.0").to_string(), "1.0");
    assert_eq!(pv("1.0_1").to_string(), "1.0_1");
    assert_eq!(pv("1.0_0").to_string(), "1.0");
    assert_eq!(pv("HEAD_1").to_string(), "HEAD_1");
    assert_eq!(pv("HEAD-ffffff_1").to_string(), "HEAD-ffffff_1");

    let p = pv("1.2.3_4");
    assert_eq!(p.version.as_str(), "1.2.3");
    assert_eq!(p.revision, 4);

    // LAST underscore-digit suffix
    let p = pv("1.0_1_2");
    assert_eq!(p.version.as_str(), "1.0_1");
    assert_eq!(p.revision, 2);
}

#[test]
fn pkg_version_revision_overflow_error() {
    let huge = format!("1.0_{}", u64::from(u32::MAX) + 1);
    let err = match PkgVersion::from_str(&huge) {
        Err(e) => e,
        Ok(v) => panic!("expected overflow error, got {v}"),
    };
    assert!(matches!(err, TypeError::InvalidPkgVersion(ref s) if s == &huge));
}

#[test]
fn pkg_version_empty_rejected() {
    let err = match PkgVersion::from_str("") {
        Err(e) => e,
        Ok(v) => panic!("expected empty rejection, got {v}"),
    };
    assert!(matches!(err, TypeError::InvalidPkgVersion(ref s) if s.is_empty()));
}

/// URL/stem detection oracle from `version_spec.rb` `describe "::detect"`.
/// Transcribed detect rows: 103. Parser families: 26 (`VERSION_PARSERS`).
#[test]
fn version_spec_detect_oracle() {
    let rows: &[(&str, &str, Option<&str>)] = &[
        ("1.14", "https://brew.sh/foo.bar.la.1.14.zip", None),
        ("1.1", "https://brew.sh/grc_1.1.tar.gz", None),
        ("1.39.0", "https://brew.sh/boost_1_39_0.tar.bz2", None),
        (
            "R13B",
            "https://erlang.org/download/otp_src_R13B.tar.gz",
            None,
        ),
        (
            "R15B01",
            "https://github.com/erlang/otp/tarball/OTP_R15B01",
            None,
        ),
        (
            "R15B03-1",
            "https://github.com/erlang/otp/tarball/OTP_R15B03-1",
            None,
        ),
        (
            "9.04",
            "https://kent.dl.sourceforge.net/sourceforge/p7zip/p7zip_9.04_src_all.tar.bz2",
            None,
        ),
        (
            "1.1.4",
            "https://github.com/sam-github/libnet/tarball/libnet-1.1.4",
            None,
        ),
        (
            "0.7.1",
            "https://codeload.github.com/gsamokovarov/jump/tar.gz/v0.7.1",
            None,
        ),
        (
            "1.0-beta7",
            "https://camaya.net/download/gloox-1.0-beta7.tar.bz2",
            None,
        ),
        (
            "1.10-beta",
            "http://sphinxsearch.com/downloads/sphinx-1.10-beta.tar.gz",
            None,
        ),
        (
            "1.23",
            "https://kent.dl.sourceforge.net/sourceforge/astyle/astyle_1.23_macosx.tar.gz",
            None,
        ),
        (
            "3.1",
            "http://www.sfr-fresh.com/linux/misc/dos2unix-3.1.tar.gz",
            None,
        ),
        ("1.1-2", "https://brew.sh/foo-arse-1.1-2.tar.gz", None),
        ("3.3.04-1", "https://brew.sh/3.3.04-1.tar.gz", None),
        ("1.2-20200102", "https://brew.sh/v1.2-20200102.tar.gz", None),
        ("3.6.6-0.2", "https://brew.sh/v3.6.6-0.2.tar.gz", None),
        ("45", "https://brew.sh/foo_bar.45.tar.gz", None),
        ("45", "https://brew.sh/foo_bar45.tar.gz", None),
        ("1.2.3", "https://brew.sh/foo-bar-la.1.2.3.tar.gz", None),
        ("1.21", "https://brew.sh/foo_bar-1.21.tar.gz", None),
        (
            "1.21",
            "https://sourceforge.net/foo_bar-1.21.tar.gz/download",
            None,
        ),
        ("1.21", "https://sf.net/foo_bar-1.21.tar.gz/download", None),
        ("1.0.5", "https://github.com/lloyd/yajl/tarball/1.0.5", None),
        (
            "1.2.34",
            "https://github.com/lloyd/yajl/tarball/v1.2.34",
            None,
        ),
        ("0.15.1b", "https://brew.sh/mad-0.15.1b.tar.gz", None),
        (
            "398-2",
            "https://kent.dl.sourceforge.net/sourceforge/lame/lame-398-2.tar.gz",
            None,
        ),
        (
            "1.9.1-p243",
            "ftp://ftp.ruby-lang.org/pub/ruby/1.9/ruby-1.9.1-p243.tar.gz",
            None,
        ),
        (
            "0.80.2",
            "http://www.alcyone.com/binaries/omega/omega-0.80.2-src.tar.gz",
            None,
        ),
        (
            "1.2.2rc1",
            "https://downloads.xiph.org/releases/vorbis/libvorbis-1.2.2rc1.tar.bz2",
            None,
        ),
        (
            "1.8.0-rc1",
            "https://ftp.mozilla.org/pub/mozilla.org/js/js-1.8.0-rc1.tar.gz",
            None,
        ),
        (
            "3.0.9b",
            "http://rephial.org/downloads/3.0/angband-3.0.9b-src.tar.gz",
            None,
        ),
        (
            "1.4.14b",
            "https://www.monkey.org/~provos/libevent-1.4.14b-stable.tar.gz",
            None,
        ),
        (
            "3.03",
            "https://ftp.de.debian.org/debian/pool/main/s/sl/sl_3.03.orig.tar.gz",
            None,
        ),
        (
            "1.01b",
            "https://ftp.de.debian.org/debian/pool/main/m/mmv/mmv_1.01b.orig.tar.gz",
            None,
        ),
        (
            "1",
            "https://deb.debian.org/debian/pool/main/e/example/example_1.orig.tar.gz",
            None,
        ),
        (
            "20040914",
            "https://deb.debian.org/debian/pool/main/e/example/example_20040914.orig.tar.gz",
            None,
        ),
        (
            "4.8.0",
            "https://homebrew.bintray.com/bottles/qt-4.8.0.lion.bottle.tar.gz",
            None,
        ),
        (
            "4.8.1",
            "https://homebrew.bintray.com/bottles/qt-4.8.1.lion.bottle.1.tar.gz",
            None,
        ),
        (
            "R15B",
            "https://homebrew.bintray.com/bottles/erlang-R15B.lion.bottle.tar.gz",
            None,
        ),
        (
            "R15B01",
            "https://homebrew.bintray.com/bottles/erlang-R15B01.mountain_lion.bottle.tar.gz",
            None,
        ),
        (
            "R15B03-1",
            "https://homebrew.bintray.com/bottles/erlang-R15B03-1.mountainlion.bottle.tar.gz",
            None,
        ),
        (
            "6.7.5-7",
            "https://downloads.sf.net/project/machomebrew/mirror/ImageMagick-6.7.5-7.tar.bz2",
            None,
        ),
        (
            "6.7.5-7",
            "https://homebrew.bintray.com/bottles/imagemagick-6.7.5-7.lion.bottle.tar.gz",
            None,
        ),
        (
            "6.7.5-7",
            "https://homebrew.bintray.com/bottles/imagemagick-6.7.5-7.lion.bottle.1.tar.gz",
            None,
        ),
        (
            "2017-04-17",
            "https://brew.sh/dada-v2017-04-17.tar.gz",
            None,
        ),
        (
            "1.3.0-beta.1",
            "https://registry.npmjs.org/@angular/cli/-/cli-1.3.0-beta.1.tgz",
            None,
        ),
        (
            "2.074.0-beta1",
            "https://github.com/dlang/dmd/archive/v2.074.0-beta1.tar.gz",
            None,
        ),
        (
            "2.074.0-rc1",
            "https://github.com/dlang/dmd/archive/v2.074.0-rc1.tar.gz",
            None,
        ),
        (
            "5.0.0-alpha10",
            "https://github.com/premake/premake-core/releases/download/v5.0.0-alpha10/premake-5.0.0-alpha10-src.zip",
            None,
        ),
        (
            "1.486",
            "https://mirrors.jenkins-ci.org/war/1.486/jenkins.war",
            None,
        ),
        (
            "0.10.11",
            "https://github.com/hechoendrupal/DrupalConsole/releases/download/0.10.11/drupal.phar",
            None,
        ),
        (
            "1.9.293",
            "https://github.com/clojure/clojurescript/releases/download/r1.9.293/cljs.jar",
            None,
        ),
        (
            "0.6.1",
            "https://github.com/fibjs/fibjs/releases/download/v0.6.1/fullsrc.zip",
            None,
        ),
        (
            "1.9",
            "https://wwwlehre.dhbw-stuttgart.de/~sschulz/WORK/E_DOWNLOAD/V_1.9/E.tgz",
            None,
        ),
        (
            "2.3.2.0",
            "https://github.com/JustArchi/ArchiSteamFarm/releases/download/2.3.2.0/ASF.zip",
            None,
        ),
        (
            "1.7.5.2",
            "https://people.gnome.org/~newren/eg/download/1.7.5.2/eg",
            None,
        ),
        (
            "3.4",
            "https://www.antlr.org/download/antlr-3.4-complete.jar",
            None,
        ),
        (
            "9.2",
            "https://cdn.nuxeo.com/nuxeo-9.2/nuxeo-server-9.2-tomcat.zip",
            None,
        ),
        (
            "0.181",
            "https://search.maven.org/remotecontent?filepath=com/facebook/presto/presto-cli/0.181/presto-cli-0.181-executable.jar",
            None,
        ),
        (
            "1.2.3",
            "https://search.maven.org/remotecontent?filepath=org/apache/orc/orc-tools/1.2.3/orc-tools-1.2.3-uber.jar",
            None,
        ),
        (
            "1.2.0-rc2",
            "https://www.apache.org/dyn/closer.cgi?path=/cassandra/1.2.0/apache-cassandra-1.2.0-rc2-bin.tar.gz",
            None,
        ),
        ("8d", "https://www.ijg.org/files/jpegsrc.v8d.tar.gz", None),
        (
            "7.0.4",
            "https://www.haskell.org/ghc/dist/7.0.4/ghc-7.0.4-x86_64-apple-darwin.tar.bz2",
            None,
        ),
        (
            "7.0.4",
            "https://www.haskell.org/ghc/dist/7.0.4/ghc-7.0.4-i386-apple-darwin.tar.bz2",
            None,
        ),
        (
            "1.4.1",
            "https://pypy.org/download/pypy-1.4.1-osx.tar.bz2",
            None,
        ),
        (
            "0.9.8s",
            "https://www.openssl.org/source/openssl-0.9.8s.tar.gz",
            None,
        ),
        (
            "1.5E",
            "ftp://ftp.visi.com/users/hawkeyd/X/Xaw3d-1.5E.tar.gz",
            None,
        ),
        (
            "2.0.863",
            "https://downloads.sourceforge.net/project/assimp/assimp-2.0/assimp--2.0.863-sdk.zip",
            None,
        ),
        (
            "20c",
            "https://common-lisp.net/project/cmucl/downloads/release/20c/cmucl-20c-x86-darwin.tar.bz2",
            None,
        ),
        (
            "2.1.0beta",
            "https://downloads.sourceforge.net/project/fann/fann/2.1.0beta/fann-2.1.0beta.zip",
            None,
        ),
        (
            "2.0.1",
            "ftp://iges.org/grads/2.0/grads-2.0.1-bin-darwin9.8-intel.tar.gz",
            None,
        ),
        ("2.08", "https://haxe.org/file/haxe-2.08-osx.tar.gz", None),
        (
            "2007f",
            "ftp://ftp.cac.washington.edu/imap/imap-2007f.tar.gz",
            None,
        ),
        (
            "3.3.12ga7",
            "https://downloads.sourceforge.net/project/x3270/x3270/3.3.12ga7/suite3270-3.3.12ga7-src.tgz",
            None,
        ),
        (
            "2.9h",
            "http://www.gedanken.demon.co.uk/download-wwwoffle/wwwoffle-2.9h.tgz",
            None,
        ),
        (
            "1.3.6p2",
            "http://synergy.googlecode.com/files/synergy-1.3.6p2-MacOSX-Universal.zip",
            None,
        ),
        (
            "20120731",
            "https://downloads.sourceforge.net/project/fontforge/fontforge-source/fontforge_full-20120731-b.tar.bz2",
            None,
        ),
        (
            "2011.10",
            "https://github.com/downloads/ezsystems/ezpublish-legacy/ezpublish_community_project-2011.10-with_ezc.tar.bz2",
            None,
        ),
        (
            "2.4c",
            "http://loop-aes.sourceforge.net/aespipe/aespipe-v2.4c.tar.bz2",
            None,
        ),
        (
            "0.9.17",
            "https://ftpmirror.gnu.org/libmicrohttpd/libmicrohttpd-0.9.17-w32.zip",
            None,
        ),
        (
            "1.29",
            "https://ftpmirror.gnu.org/libidn/libidn-1.29-win64.zip",
            None,
        ),
        (
            "0.35.1",
            "https://github.com/barricklab/breseq/releases/download/v0.35.1/breseq-0.35.1.Source.tar.gz",
            None,
        ),
        (
            "20.0.1",
            "https://download.jboss.org/wildfly/20.0.1.Final/wildfly-20.0.1.Final.tar.gz",
            None,
        ),
        (
            "2.10.0",
            "https://github.com/trinityrnaseq/trinityrnaseq/releases/download/v2.10.0/trinityrnaseq-v2.10.0.FULL.tar.gz",
            None,
        ),
        (
            "4.0.18-1",
            "https://ftpmirror.gnu.org/mtools/mtools-4.0.18-1.i686.rpm",
            None,
        ),
        (
            "5.5.7-5",
            "https://ftpmirror.gnu.org/autogen/autogen-5.5.7-5.i386.rpm",
            None,
        ),
        (
            "2.8",
            "https://ftpmirror.gnu.org/libtasn1/libtasn1-2.8-x86.zip",
            None,
        ),
        (
            "2.8",
            "https://ftpmirror.gnu.org/libtasn1/libtasn1-2.8-x64.zip",
            None,
        ),
        (
            "4.0.18",
            "https://ftpmirror.gnu.org/mtools/mtools_4.0.18_i386.deb",
            None,
        ),
        (
            "2.18.3",
            "https://opam.ocaml.org/archives/lablgtk.2.18.3+opam.tar.gz",
            None,
        ),
        (
            "1.9",
            "https://opam.ocaml.org/archives/sha.1.9+opam.tar.gz",
            None,
        ),
        (
            "0.99.2",
            "https://opam.ocaml.org/archives/ppx_tools.0.99.2+opam.tar.gz",
            None,
        ),
        (
            "1.0.2",
            "https://opam.ocaml.org/archives/easy-format.1.0.2+opam.tar.gz",
            None,
        ),
        ("1.8.12", "https://waf.io/waf-1.8.12", None),
        (
            "0.7.1",
            "https://codeload.github.com/gsamokovarov/jump/tar.gz/v0.7.1",
            None,
        ),
        (
            "0.9.1234",
            "https://my.datomic.com/downloads/free/0.9.1234",
            None,
        ),
        ("1.2.3", "https://my.datomic.com/downloads/free/1.2.3", None),
        (
            "6-20151227",
            "ftp://gcc.gnu.org/pub/gcc/snapshots/6-20151227/gcc-6-20151227.tar.bz2",
            None,
        ),
        (
            "7.1.10",
            "https://php.net/get/php-7.1.10.tar.gz/from/this/mirror",
            None,
        ),
        (
            "1.2.3",
            "https://github.com/foo/bar.git",
            Some("v1.2.3-stable"),
        ),
        (
            "1.2.3-beta1",
            "https://github.com/foo/bar.git",
            Some("v1.2.3-beta1"),
        ),
        (
            "3.2",
            "https://github.com/dvorka-oss/hstr/releases/download/v3.2/hstr-3.2.0-tarball.tgz",
            None,
        ),
    ];
    assert_eq!(rows.len(), 103, "detect oracle row count drift");
    for (expected, url, tag) in rows {
        let got = match tag {
            Some(tag) => Version::detect_with_tag(url, tag),
            None => Version::detect(url),
        };
        assert!(got.detected_from_url(), "flag set for {url}");
        assert_eq!(got.as_str(), *expected, "detect({url}) tag={tag:?}");
        assert_eq!(got, v(expected));
    }
}

#[test]
fn detect_sets_flag_and_plain_parse_does_not() {
    let plain = v("1.0.0");
    assert!(!plain.detected_from_url());
    let detected = Version::detect("https://example.org/archive-1.0.0.tar.gz");
    assert!(detected.detected_from_url());
    assert_eq!(detected.as_str(), "1.0.0");
}

#[test]
fn detect_returns_null_when_no_parser_matches() {
    let n = Version::detect("https://example.org/totally-unversioned/");
    assert!(n.is_null(), "expected null, got {}", n.as_str());
    assert!(!n.detected_from_url());
}

// ===========================================================================
// Finite exhaustive Version ordering / equality / hash property tests.
//
// Corpus: every unique left/right string appearing in
// `version_spec_comparison_oracle` (the 75-row table) plus required edge
// cases — empty, delimiters-only, bare 0/0.0, bare a/b, rc/p1/p2/p10/q,
// post forms, bare numerics, alpha/beta case aliases, HEAD forms, and the
// non-HEAD look-alikes HEADLESS / HEAD2.
//
// Laws are checked exhaustively over this finite corpus:
//   L1  equality symmetry      (a == b) == (b == a)              — all pairs
//   L2  cmp reversal           a.cmp(b) == b.cmp(a).reverse()    — all pairs
//   L3  cmp/Eq equivalence     (a.cmp(b) == Equal) == (a == b)   — all pairs
//   L4  hash equality          a == b  ==>  hash(a) == hash(b)   — all pairs
//                              and a HashSet that inserted a also contains b
//   L5  total-order trans.     a <= b && b <= c  ==>  a <= c     — all triples
// ===========================================================================

/// The finite exhaustive corpus shared by every property test below.
fn version_property_corpus() -> Vec<(&'static str, Version)> {
    let strs: &[&str] = &[
        // --- unique left/right values from the 75-row comparison oracle ---
        "",
        "1",
        "0",
        "0.1",
        "0.1.0",
        "0.2",
        "1.2.3",
        "1.2.2",
        "1.2.4",
        "1.2.4.1",
        "1.2.3alpha4",
        "1.2.3beta2",
        "1.2.3rc3",
        "1.2.3-p34",
        "HEAD",
        "HEAD-abcdef",
        "HEAD-fedcba",
        "1.2.3alpha",
        "1.2.3a",
        "1.2.3a4",
        "1.2.3A4",
        "1.2.3alpha3",
        "1.2.3alpha5",
        "1.2.3alpha10",
        "1.2.3b2",
        "1.2.3B2",
        "1.2.3beta1",
        "1.2.3beta3",
        "1.2.3beta10",
        "1.2.3pre9",
        "1.2.3PRE9",
        "1.2.3pre8",
        "1.2.3pre10",
        "1.2.3pre3",
        "1.2.3alpha2",
        "1.2.3beta5",
        "1.2.3rc2",
        "1.2.3-p2",
        "1.2.3RC3",
        "1.2.3rc4",
        "1.2.3rc10",
        "1.2.3-P34",
        "1.2.3-p33",
        "1.2.3-p35",
        "1.2.3-p9",
        "1.2.3.post34",
        "1.2.3.post33",
        "1.2.3.post35",
        "1.2.3rc35",
        "1.2.3alpha35",
        "1.2.3beta35",
        "2.1.0-p194",
        "2.1-p195",
        "2.1-p194",
        "2.1.0-p195",
        "2-p194",
        // --- required edge cases beyond the oracle ---
        "...",
        "---",
        "0.0",
        "a",
        "b",
        "rc",
        "p1",
        "p2",
        "p10",
        "q",
        ".post1",
        "2",
        "10",
        "HEADLESS",
        "HEAD2",
        // --- interstitial-zero / Erlang regression fixtures ---
        "1.0.2",
        "1.2",
        "1.0.0.2",
        "R14B",
        "R14B01",
    ];
    strs.iter().map(|&s| (s, v(s))).collect()
}

/// Default-hash of a [`Version`] (uses the same canonical sequence as `Ord`).
fn version_hash(ver: &Version) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    ver.hash(&mut h);
    h.finish()
}

/// L1 — both-direction symmetry of equality: `(a == b) == (b == a)`.
#[test]
fn prop_equality_symmetry() {
    let corpus = version_property_corpus();
    for &(sa, ref a) in &corpus {
        for &(sb, ref b) in &corpus {
            assert_eq!(
                a == b,
                b == a,
                "L1 symmetry violated: ({sa:?} == {sb:?}) != ({sb:?} == {sa:?})"
            );
        }
    }
}

/// L2 — cmp reversal: `a.cmp(b) == b.cmp(a).reverse()`.
#[test]
fn prop_cmp_reversal() {
    let corpus = version_property_corpus();
    for &(sa, ref a) in &corpus {
        for &(sb, ref b) in &corpus {
            let ab = a.cmp(b);
            let ba = b.cmp(a);
            assert_eq!(
                ab,
                ba.reverse(),
                "L2 cmp reversal violated: {sa:?}.cmp({sb:?})={ab:?}, \
                 reverse of {sb:?}.cmp({sa:?})={ba:?}"
            );
        }
    }
}

/// L3 — cmp/Eq equivalence: `(a.cmp(b) == Equal) == (a == b)`.
#[test]
fn prop_cmp_eq_equivalence() {
    let corpus = version_property_corpus();
    for &(sa, ref a) in &corpus {
        for &(sb, ref b) in &corpus {
            let cmp_is_eq = a.cmp(b) == Ordering::Equal;
            let eq = a == b;
            assert_eq!(
                cmp_is_eq, eq,
                "L3 cmp/Eq equivalence violated: {sa:?}.cmp({sb:?})==Equal is {cmp_is_eq} \
                 but {sa:?} == {sb:?} is {eq}"
            );
        }
    }
}

/// L4 — hash equality: if `a == b` then `hash(a) == hash(b)` and a `HashSet`
/// that inserted `a` also `contains` `b`.
#[test]
fn prop_hash_equality_and_set_lookup() {
    let corpus = version_property_corpus();
    for &(sa, ref a) in &corpus {
        let mut set = HashSet::new();
        set.insert(a.clone());
        for &(sb, ref b) in &corpus {
            if a == b {
                assert_eq!(
                    version_hash(a),
                    version_hash(b),
                    "L4 hash mismatch for equal versions {sa:?} == {sb:?}"
                );
                assert!(
                    set.contains(b),
                    "L4 HashSet lookup failed: inserted {sa:?}, \
                     expected to contain equal {sb:?}"
                );
            }
        }
    }
}

/// L5 — total-order transitivity: for all triples, `a <= b && b <= c` implies
/// `a <= c` (covers the Equal / Less / Greater cases of a total order; the
/// equivalence-relation property of equality follows from L1–L3).
#[test]
fn prop_total_order_transitivity() {
    let corpus = version_property_corpus();
    for &(sa, ref a) in &corpus {
        for &(sb, ref b) in &corpus {
            for &(sc, ref c) in &corpus {
                let a_le_b = a.cmp(b) != Ordering::Greater;
                let b_le_c = b.cmp(c) != Ordering::Greater;
                if a_le_b && b_le_c {
                    let ac = a.cmp(c);
                    assert!(
                        ac != Ordering::Greater,
                        "L5 transitivity violated: {sa:?} <= {sb:?} and {sb:?} <= {sc:?} \
                         but {sa:?}.cmp({sc:?}) = {ac:?}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Concrete named counterexample tests pinning specific token-class orderings.
// ---------------------------------------------------------------------------

/// `0 < a < b` — a bare numeric zero (canonical-empty: sole zero has no next
/// retained token, so it is dropped) sorts below bare alphabetic string tokens
/// (class rank 6), which order lexicographically. Bare lowercase `a` stays a
/// String token (not Alpha).
#[test]
fn counterexample_zero_lt_a_lt_b() {
    assert!(v("0") < v("a"), "expected 0 < a");
    assert!(v("a") < v("b"), "expected a < b");
    assert!(v("0") < v("b"), "expected 0 < b (transitive)");
    assert_eq!(v("0").cmp(&v("a")), Ordering::Less);
    assert_eq!(v("a").cmp(&v("b")), Ordering::Less);
}

/// Position-aware zero retention + Erlang `B`→`Beta(0)` regressions.
#[test]
fn interstitial_zero_and_erlang_regressions() {
    assert!(v("1.0.2") < v("1.2"), "interstitial zero: 1.0.2 < 1.2");
    assert!(
        v("1.0.0.2") < v("1.0.2"),
        "stacked interstitial zeros: 1.0.0.2 < 1.0.2"
    );
    assert!(v("0") < v("a"), "0 < a");
    assert_ne!(
        v("0.1"),
        v("1"),
        "leading zero kept when next retained is numeric"
    );
    assert_eq!(v("0.1"), v("0.1.0"));
    assert_eq!(v("2.1.0-p194"), v("2.1-p194"));

    // Exact Ruby ascending order (version_spec.rb:283-286).
    let ascending = [
        "R13B02-1", "R13B03", "R13B04", "R14B", "R14B01", "R14B02", "R14B03", "R14B04", "R15B01",
        "R15B02", "R15B03", "R15B03-1", "R16B",
    ];
    let mut sorted: Vec<Version> = ascending.iter().map(|s| v(s)).collect();
    sorted.sort();
    let got: Vec<String> = sorted.iter().map(|x| x.to_string()).collect();
    assert_eq!(got, ascending);

    // HashSet distinction for unequal interstitial forms.
    let mut set = HashSet::new();
    set.insert(v("1.0.2"));
    set.insert(v("1.2"));
    assert_eq!(
        set.len(),
        2,
        "1.0.2 and 1.2 must remain distinct under Eq/Hash"
    );
    assert!(set.contains(&v("1.0.2")));
    assert!(set.contains(&v("1.2")));
    assert!(!set.contains(&v("1.0.0.2")));
}

/// `rc < q < p1` — a release-candidate token (class rank 3) sorts below a bare
/// string token `q` (rank 6) which sorts below a patch token `p1` (rank 7).
/// Patch revisions compare numerically, so `p1 < p2 < p10` (not lexically).
#[test]
fn counterexample_rc_lt_q_lt_p1() {
    assert!(v("rc") < v("q"), "expected rc < q");
    assert!(v("q") < v("p1"), "expected q < p1");
    assert!(v("rc") < v("p1"), "expected rc < p1 (transitive)");
    assert_eq!(v("rc").cmp(&v("q")), Ordering::Less);
    assert_eq!(v("q").cmp(&v("p1")), Ordering::Less);
    // numeric (not lexical) patch-revision ordering: p1 < p2 < p10
    assert!(v("p1") < v("p2"), "expected p1 < p2");
    assert!(v("p2") < v("p10"), "expected p2 < p10 (numeric patch rev)");
    assert!(v("p1") < v("p10"), "expected p1 < p10 (numeric patch rev)");
}
