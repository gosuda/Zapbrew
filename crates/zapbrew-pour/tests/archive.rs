//! Integration tests for secure bottle archive unpack.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::str::FromStr;

use camino::Utf8PathBuf;
use flate2::Compression;
use flate2::write::GzEncoder;
use tar::{Builder, EntryType, Header};
use tempfile::TempDir;
use zapbrew_pour::{PourError, unpack};
use zapbrew_types::{FormulaName, PkgVersion};

fn utf8_temp() -> (TempDir, Utf8PathBuf) {
    let temp = match TempDir::new() {
        Ok(temp) => temp,
        Err(err) => panic!("tempdir: {err}"),
    };
    let root = match Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) {
        Ok(root) => root,
        Err(path) => panic!("non-utf8 temp path {}", path.display()),
    };
    (temp, root)
}

fn formula_name(raw: &str) -> FormulaName {
    match FormulaName::from_str(raw) {
        Ok(name) => name,
        Err(err) => panic!("formula name {raw}: {err}"),
    }
}

fn pkg_version(raw: &str) -> PkgVersion {
    match PkgVersion::from_str(raw) {
        Ok(version) => version,
        Err(err) => panic!("pkg version {raw}: {err}"),
    }
}

fn assert_invalid_archive(err: PourError) {
    match err {
        PourError::InvalidArchive { .. } => {}
        other => panic!("expected InvalidArchive, got {other}"),
    }
}

enum TestEntry<'a> {
    Dir {
        mode: u32,
    },
    File {
        mode: u32,
        data: &'a [u8],
    },
    Symlink {
        target: &'a str,
    },
    Hardlink {
        target: &'a str,
    },
    Special {
        entry_type: EntryType,
    },
    /// Raw header path bytes (for traversal / absolute / curdir cases).
    RawFile {
        path: &'a str,
        data: &'a [u8],
    },
}

fn write_gzip_tarball(path: &Path, entries: &[(&str, TestEntry<'_>)]) {
    let file = match File::create(path) {
        Ok(file) => file,
        Err(err) => panic!("create {}: {err}", path.display()),
    };
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);

    for (name, entry) in entries {
        match entry {
            TestEntry::Dir { mode } => {
                let mut header = Header::new_gnu();
                if let Err(err) = header.set_path(name) {
                    panic!("set_path {name}: {err}");
                }
                header.set_entry_type(EntryType::Directory);
                header.set_mode(*mode);
                header.set_size(0);
                header.set_cksum();
                if let Err(err) = builder.append(&header, io::empty()) {
                    panic!("append dir {name}: {err}");
                }
            }
            TestEntry::File { mode, data } => {
                let mut header = Header::new_gnu();
                header.set_entry_type(EntryType::Regular);
                header.set_mode(*mode);
                header.set_size(data.len() as u64);
                header.set_cksum();
                if let Err(err) = builder.append_data(&mut header, name, *data) {
                    panic!("append file {name}: {err}");
                }
            }
            TestEntry::Symlink { target } => {
                let mut header = Header::new_gnu();
                header.set_entry_type(EntryType::Symlink);
                header.set_mode(0o777);
                header.set_size(0);
                if let Err(err) = header.set_link_name(target) {
                    panic!("set_link_name {target}: {err}");
                }
                header.set_cksum();
                if let Err(err) = builder.append_data(&mut header, name, io::empty()) {
                    panic!("append symlink {name}: {err}");
                }
            }
            TestEntry::Hardlink { target } => {
                let mut header = Header::new_gnu();
                header.set_entry_type(EntryType::Link);
                header.set_mode(0o644);
                header.set_size(0);
                if let Err(err) = header.set_link_name(target) {
                    panic!("set_link_name {target}: {err}");
                }
                header.set_cksum();
                if let Err(err) = builder.append_data(&mut header, name, io::empty()) {
                    panic!("append hardlink {name}: {err}");
                }
            }
            TestEntry::Special { entry_type } => {
                let mut header = Header::new_gnu();
                if let Err(err) = header.set_path(name) {
                    panic!("set_path {name}: {err}");
                }
                header.set_entry_type(*entry_type);
                header.set_mode(0o644);
                header.set_size(0);
                header.set_cksum();
                if let Err(err) = builder.append(&header, io::empty()) {
                    panic!("append special {name}: {err}");
                }
            }
            TestEntry::RawFile {
                path: raw_path,
                data,
            } => {
                let mut header = Header::new_gnu();
                let bytes = raw_path.as_bytes();
                if bytes.len() >= header.as_old().name.len() {
                    panic!("raw path too long: {raw_path}");
                }
                {
                    let name_buf = &mut header.as_old_mut().name;
                    name_buf.fill(0);
                    name_buf[..bytes.len()].copy_from_slice(bytes);
                }
                header.set_entry_type(EntryType::Regular);
                header.set_mode(0o644);
                header.set_size(data.len() as u64);
                header.set_cksum();
                if let Err(err) = builder.append(&header, *data) {
                    panic!("append raw {raw_path}: {err}");
                }
            }
        }
    }

    let encoder = match builder.into_inner() {
        Ok(encoder) => encoder,
        Err(err) => panic!("finish tar {}: {err}", path.display()),
    };
    if let Err(err) = encoder.finish() {
        panic!("finish gzip {}: {err}", path.display());
    }
}

fn write_absolute_entry_tarball(path: &Path, abs_entry: &str, data: &[u8]) {
    let file = match File::create(path) {
        Ok(file) => file,
        Err(err) => panic!("create {}: {err}", path.display()),
    };
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);
    builder.preserve_absolute(true);

    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(data.len() as u64);
    if let Err(err) = header.set_path_absolute(abs_entry) {
        panic!("set_path_absolute {abs_entry}: {err}");
    }
    header.set_cksum();
    if let Err(err) = builder.append(&header, data) {
        panic!("append absolute {abs_entry}: {err}");
    }

    let encoder = match builder.into_inner() {
        Ok(encoder) => encoder,
        Err(err) => panic!("finish tar {}: {err}", path.display()),
    };
    if let Err(err) = encoder.finish() {
        panic!("finish gzip {}: {err}", path.display());
    }
}

#[test]
fn unpack_valid_layout_extracts_keg_tree() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let tarball = root.join("foo--1.2.3.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.2.3");

    write_gzip_tarball(
        tarball.as_std_path(),
        &[
            ("foo/1.2.3", TestEntry::Dir { mode: 0o755 }),
            ("foo/1.2.3/bin", TestEntry::Dir { mode: 0o755 }),
            (
                "foo/1.2.3/bin/foo",
                TestEntry::File {
                    mode: 0o755,
                    data: b"#!/bin/sh\necho foo\n",
                },
            ),
            (
                "foo/1.2.3/README",
                TestEntry::File {
                    mode: 0o644,
                    data: b"hello\n",
                },
            ),
            (
                "foo/1.2.3/bin/alias",
                TestEntry::Symlink {
                    target: "../README",
                },
            ),
        ],
    );

    let keg = match unpack(&tarball, &cellar, &name, &version) {
        Ok(keg) => keg,
        Err(err) => panic!("unpack valid bottle: {err}"),
    };

    assert_eq!(keg.path(), cellar.join("foo/1.2.3"));
    assert!(keg.path().join("bin/foo").is_file());
    assert!(keg.path().join("README").is_file());
    let alias = keg.path().join("bin/alias");
    let target = match fs::read_link(alias.as_std_path()) {
        Ok(target) => target,
        Err(err) => panic!("read_link alias: {err}"),
    };
    assert_eq!(target, Path::new("../README"));
}

#[test]
fn unpack_rejects_wrong_name_version_prefix() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let tarball = root.join("wrong-prefix.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    write_gzip_tarball(
        tarball.as_std_path(),
        &[
            ("bar/1.0.0", TestEntry::Dir { mode: 0o755 }),
            (
                "bar/1.0.0/bin/bar",
                TestEntry::File {
                    mode: 0o755,
                    data: b"nope\n",
                },
            ),
        ],
    );

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("wrong prefix must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }
    assert!(
        !cellar.join("bar").exists(),
        "rejected unpack must not create wrong-prefix tree"
    );
    assert!(
        !cellar.join("foo").exists(),
        "rejected unpack must not create requested keg"
    );
}

#[test]
fn unpack_rejects_parent_directory_components() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let tarball = root.join("dotdot.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    write_gzip_tarball(
        tarball.as_std_path(),
        &[(
            "ignored",
            TestEntry::RawFile {
                path: "foo/1.0.0/../../../tmp/evil",
                data: b"pwned\n",
            },
        )],
    );

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("parent-dir path must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }
    if cellar.exists() {
        let mut entries = match fs::read_dir(cellar.as_std_path()) {
            Ok(entries) => entries,
            Err(err) => panic!("read cellar after reject: {err}"),
        };
        match entries.next() {
            None => {}
            Some(Ok(entry)) => panic!("cellar unexpectedly contains {}", entry.path().display()),
            Some(Err(err)) => panic!("read cellar entry: {err}"),
        }
    }
}

#[test]
fn unpack_rejects_absolute_entry_paths() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let tarball = root.join("absolute.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    write_absolute_entry_tarball(tarball.as_std_path(), "/etc/zapbrew-evil", b"nope\n");

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("absolute path must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }
    assert!(!Path::new("/etc/zapbrew-evil").exists());
}

#[test]
fn unpack_rejects_current_directory_components() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let tarball = root.join("curdir.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    write_gzip_tarball(
        tarball.as_std_path(),
        &[(
            "ignored",
            TestEntry::RawFile {
                path: "foo/1.0.0/./bin/foo",
                data: b"nope\n",
            },
        )],
    );

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("curdir path must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }
}

#[test]
fn unpack_rejects_escaping_symlink_and_hardlink() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    let symlink_tar = root.join("escape-symlink.bottle.tar.gz");
    write_gzip_tarball(
        symlink_tar.as_std_path(),
        &[
            ("foo/1.0.0", TestEntry::Dir { mode: 0o755 }),
            (
                "foo/1.0.0/link",
                TestEntry::Symlink {
                    target: "../../../../etc/passwd",
                },
            ),
        ],
    );
    match unpack(&symlink_tar, &cellar, &name, &version) {
        Ok(_) => panic!("escaping symlink must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }

    let hardlink_tar = root.join("escape-hardlink.bottle.tar.gz");
    write_gzip_tarball(
        hardlink_tar.as_std_path(),
        &[
            ("foo/1.0.0", TestEntry::Dir { mode: 0o755 }),
            (
                "foo/1.0.0/bin/foo",
                TestEntry::File {
                    mode: 0o755,
                    data: b"ok\n",
                },
            ),
            (
                "foo/1.0.0/evil",
                TestEntry::Hardlink {
                    target: "foo/1.0.0/../../outside",
                },
            ),
        ],
    );
    match unpack(&hardlink_tar, &cellar, &name, &version) {
        Ok(_) => panic!("escaping hardlink must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }
}

#[test]
fn unpack_rejects_nested_symlink_escape_before_mutation() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    // Sibling keg file that must remain byte-identical if preflight aborts.
    let sibling = cellar.join("victim/9.9.9/keep");
    if let Err(err) = fs::create_dir_all(sibling.parent().expect("sibling parent").as_std_path()) {
        panic!("create sibling keg: {err}");
    }
    const SIBLING_BYTES: &[u8] = b"untouched-sibling-bytes\n";
    if let Err(err) = fs::write(sibling.as_std_path(), SIBLING_BYTES) {
        panic!("write sibling file: {err}");
    }

    let tarball = root.join("symlink-nest.bottle.tar.gz");
    // Lexical nest: s1 -> . keeps targets looking in-keg; s2 -> ../.. then a
    // later write would follow canonicalize out of the staged keg.
    write_gzip_tarball(
        tarball.as_std_path(),
        &[
            ("foo/1.0.0", TestEntry::Dir { mode: 0o755 }),
            ("foo/1.0.0/nested", TestEntry::Dir { mode: 0o755 }),
            ("foo/1.0.0/nested/s1", TestEntry::Symlink { target: "." }),
            (
                "foo/1.0.0/nested/s1/s2",
                TestEntry::Symlink { target: "../.." },
            ),
            (
                "foo/1.0.0/nested/s1/s2/pwned",
                TestEntry::File {
                    mode: 0o644,
                    data: b"escaped\n",
                },
            ),
        ],
    );

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("nested symlink escape must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }

    assert!(
        !cellar.join("foo").exists(),
        "exploit fixture must fail before any extraction mutation"
    );
    let kept = match fs::read(sibling.as_std_path()) {
        Ok(bytes) => bytes,
        Err(err) => panic!("read sibling after reject: {err}"),
    };
    assert_eq!(
        kept, SIBLING_BYTES,
        "sibling keg file must stay byte-identical"
    );

    // No cellar-root symlink from the nest (e.g. s2 landing beside formula dirs).
    let cellar_entries = match fs::read_dir(cellar.as_std_path()) {
        Ok(entries) => entries,
        Err(err) => panic!("read cellar after reject: {err}"),
    };
    for entry in cellar_entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => panic!("read cellar entry: {err}"),
        };
        let meta = match fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(err) => panic!("symlink_metadata {}: {err}", entry.path().display()),
        };
        assert!(
            !meta.file_type().is_symlink(),
            "no cellar-root symlink may appear ({})",
            entry.path().display()
        );
    }
}

#[test]
fn unpack_rejects_device_and_fifo_entries() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    for (label, entry_type) in [
        ("fifo", EntryType::Fifo),
        ("char", EntryType::Char),
        ("block", EntryType::Block),
    ] {
        let tarball = root.join(format!("{label}.bottle.tar.gz"));
        let special_path = format!("foo/1.0.0/{label}");
        write_gzip_tarball(
            tarball.as_std_path(),
            &[
                ("foo/1.0.0", TestEntry::Dir { mode: 0o755 }),
                (special_path.as_str(), TestEntry::Special { entry_type }),
            ],
        );
        match unpack(&tarball, &cellar, &name, &version) {
            Ok(_) => panic!("{label} entry must be rejected"),
            Err(err) => assert_invalid_archive(err),
        }
    }
}

#[test]
fn unpack_late_bad_entry_does_not_partially_extract() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let marker = root.join("outside-marker");
    let tarball = root.join("late-bad.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    if let Err(err) = fs::write(marker.as_std_path(), b"keep\n") {
        panic!("write marker: {err}");
    }

    write_gzip_tarball(
        tarball.as_std_path(),
        &[
            ("foo/1.0.0", TestEntry::Dir { mode: 0o755 }),
            ("foo/1.0.0/bin", TestEntry::Dir { mode: 0o755 }),
            (
                "foo/1.0.0/bin/foo",
                TestEntry::File {
                    mode: 0o755,
                    data: b"first\n",
                },
            ),
            (
                "foo/1.0.0/share/doc/foo",
                TestEntry::File {
                    mode: 0o644,
                    data: b"docs\n",
                },
            ),
            (
                "ignored",
                TestEntry::RawFile {
                    path: "foo/1.0.0/../../../escape",
                    data: b"late\n",
                },
            ),
        ],
    );

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("late bad entry must fail preflight"),
        Err(err) => assert_invalid_archive(err),
    }

    assert!(
        !cellar.join("foo").exists(),
        "preflight failure must not leave a partial keg"
    );
    assert!(marker.exists(), "preflight must not mutate unrelated paths");
}

#[test]
fn unpack_sets_owner_writable_without_world_writable() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let tarball = root.join("modes.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    write_gzip_tarball(
        tarball.as_std_path(),
        &[
            ("foo/1.0.0", TestEntry::Dir { mode: 0o555 }),
            ("foo/1.0.0/lib", TestEntry::Dir { mode: 0o555 }),
            (
                "foo/1.0.0/lib/libfoo.a",
                TestEntry::File {
                    mode: 0o444,
                    data: b"!\n",
                },
            ),
        ],
    );

    let keg = match unpack(&tarball, &cellar, &name, &version) {
        Ok(keg) => keg,
        Err(err) => panic!("unpack modes bottle: {err}"),
    };

    for rel in ["", "lib"] {
        let dir = if rel.is_empty() {
            keg.path().to_owned()
        } else {
            keg.path().join(rel)
        };
        let meta = match fs::metadata(dir.as_std_path()) {
            Ok(meta) => meta,
            Err(err) => panic!("metadata {dir}: {err}"),
        };
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o200,
            0o200,
            "{dir} must be owner-writable, got {mode:o}"
        );
        assert_eq!(
            mode & 0o002,
            0,
            "{dir} must not be world-writable, got {mode:o}"
        );
    }
}

#[test]
fn unpack_rejects_non_gzip_magic() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let tarball = root.join("not-gzip.bottle.tar.gz");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    if let Err(err) = fs::write(tarball.as_std_path(), b"not a gzip file") {
        panic!("write decoy: {err}");
    }

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("non-gzip must be rejected"),
        Err(err) => assert_invalid_archive(err),
    }
}

#[test]
fn unpack_rejects_directory_below_archive_symlink_before_mutation() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let name = formula_name("foo");
    let version = pkg_version("1.0.0");

    let tarball = root.join("dir-under-symlink.bottle.tar.gz");
    // Symlink ancestor followed by a directory child must fail in preflight
    // before any extraction; the target keg must remain absent/empty.
    write_gzip_tarball(
        tarball.as_std_path(),
        &[
            ("foo/1.0.0", TestEntry::Dir { mode: 0o755 }),
            (
                "foo/1.0.0/nested",
                TestEntry::Symlink {
                    // In-keg target so validate_link accepts the symlink itself;
                    // the directory child below it is what must be rejected.
                    target: ".",
                },
            ),
            ("foo/1.0.0/nested/child", TestEntry::Dir { mode: 0o755 }),
        ],
    );

    match unpack(&tarball, &cellar, &name, &version) {
        Ok(_) => panic!("directory under archive symlink must be rejected"),
        Err(err) => match err {
            PourError::InvalidArchive { reason, .. } => {
                assert!(
                    reason.contains("non-directory") || reason.contains("symlink"),
                    "reason={reason}"
                );
            }
            other => panic!("expected InvalidArchive, got {other}"),
        },
    }

    assert!(
        !cellar.join("foo").exists(),
        "target keg must remain absent after preflight rejection"
    );
    if cellar.exists() {
        let mut entries = match fs::read_dir(cellar.as_std_path()) {
            Ok(entries) => entries,
            Err(err) => panic!("read cellar after reject: {err}"),
        };
        match entries.next() {
            None => {}
            Some(Ok(entry)) => panic!("cellar unexpectedly contains {}", entry.path().display()),
            Some(Err(err)) => panic!("read cellar entry: {err}"),
        }
    }
}
