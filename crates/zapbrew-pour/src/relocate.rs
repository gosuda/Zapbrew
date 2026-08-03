//! Bottle relocation: rewrite the build-time prefix placeholders in poured keg
//! files to the target [`Env`] layout.
//!
//! The whole keg is planned before a single byte is written: text files, plain
//! binaries, ELF images, and Mach-O images are each transformed in memory, the
//! mutated bytes are re-parsed with `object`, and only then are they promoted
//! with a temp-write + fsync + atomic rename. Any planning failure (a segment
//! that cannot fit, an unsupported ELF layout, a post-mutation parse failure)
//! aborts the whole operation before touching disk, so the on-disk keg is left
//! byte-for-byte unchanged.
//!
//! ELF relocation follows the binding contract in `.outline/sdd/contracts.md`
//! ("Binary relocation"): `object` read locates/validates, but mutation is a
//! capacity-preserving in-place byte patch — never a wholesale re-serialization
//! through `object::write`. A `DT_RUNPATH`/`PT_INTERP` string that no longer
//! fits its slot is appended inside an extended final `PT_LOAD`, with
//! `DT_STRTAB`/`DT_STRSZ` (and the `.dynstr` section header, when present)
//! repointed. Structured ELF rewriting is 64-bit only (every Homebrew Linux
//! bottle is ELF64). Mach-O images get a structured load-command pass that
//! rewrites `LC_ID_DYLIB`/`LC_LOAD_DYLIB`/`LC_RPATH` strings in place, using
//! the trailing NUL padding inside each command's `cmdsize` as slot capacity;
//! other binaries receive the generic NUL-padded pass alone.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use object::BinaryFormat;
use object::read::File as ObjectFile;
use zapbrew_prefix::{CommandRunner, CommandSpec, Env};
use zapbrew_types::BottleTag;

use crate::error::PourError;
use crate::types::RelocationReport;

/// Bottle `cellar` value that opts a bottle out of relocation entirely.
const SKIP_RELOCATION: &str = ":any_skip_relocation";

/// Relocate every file under `keg` from the bottle's build placeholders to the
/// live `env` layout.
///
/// `cellar_field` is the bottle's declared `cellar`; `:any_skip_relocation`
/// short-circuits to an empty report. `runner` runs `codesign` for modified
/// Mach-O images (content-detected, so the seam is exercised host-independently
/// in tests). Returns the keg-relative paths of every file whose bytes changed.
pub fn relocate(
    keg: &zapbrew_prefix::Keg,
    env: &Env,
    cellar_field: &str,
    runner: &dyn CommandRunner,
) -> Result<RelocationReport, PourError> {
    if cellar_field.trim() == SKIP_RELOCATION {
        return Ok(RelocationReport::default());
    }

    let subs = placeholder_subs(env);
    let new_prefix = env.prefix.as_str();
    let keg_root = keg.path();
    let keg_name = keg.name().name();
    let is_glibc = is_glibc(keg_name);
    let is_gcc = is_gcc(keg_name);
    let ld_so_readable = Utf8Path::new(new_prefix).join("lib/ld.so").exists();

    let groups = collect_hardlink_groups(keg_root)?;

    // Plan phase: transform every representative file in memory. Any error here
    // returns before the apply phase, leaving the keg unchanged.
    let mut plans: Vec<FilePlan> = Vec::new();
    for group in &groups {
        let representative = &group.representative;
        let original = fs::read(representative.as_std_path())
            .map_err(|source| PourError::io("read", representative.clone(), source))?;
        let mode = fs::metadata(representative.as_std_path())
            .map_err(|source| PourError::io("stat", representative.clone(), source))?
            .mode();

        let outcome = plan_file(
            &original,
            representative,
            &subs,
            new_prefix,
            is_glibc,
            is_gcc,
            ld_so_readable,
        )?;

        if let Some((new_bytes, codesign)) = outcome {
            let relative = representative
                .strip_prefix(keg_root)
                .map(Utf8Path::to_path_buf)
                .unwrap_or_else(|_| representative.clone());
            plans.push(FilePlan {
                representative: representative.clone(),
                relative,
                new_bytes,
                mode,
                siblings: group.siblings.clone(),
                codesign,
            });
        }
    }

    // Apply phase: nothing here can reject the mutation on content grounds.
    let mut report = RelocationReport::default();
    for plan in plans {
        write_atomic(&plan.representative, &plan.new_bytes, plan.mode)?;
        // The atomic rename gives the representative a fresh inode, breaking the
        // original hardlink group; re-point every sibling at the new content.
        for sibling in &plan.siblings {
            fs::remove_file(sibling.as_std_path())
                .map_err(|source| PourError::io("remove", sibling.clone(), source))?;
            fs::hard_link(plan.representative.as_std_path(), sibling.as_std_path())
                .map_err(|source| PourError::io("hardlink", sibling.clone(), source))?;
        }
        if plan.codesign {
            codesign(runner, &plan.representative)?;
        }
        report.changed_files.push(plan.relative);
    }

    Ok(report)
}

/// A planned, in-memory rewrite of one representative file and its hardlinks.
struct FilePlan {
    representative: Utf8PathBuf,
    relative: Utf8PathBuf,
    new_bytes: Vec<u8>,
    mode: u32,
    siblings: Vec<Utf8PathBuf>,
    codesign: bool,
}

/// One inode's representative path plus the sibling paths hardlinked to it.
struct HardlinkGroup {
    representative: Utf8PathBuf,
    siblings: Vec<Utf8PathBuf>,
}

/// Recursively gather regular files under `root` (via `std::fs::read_dir`, no
/// `walkdir`), grouping hardlinks by `(dev, ino)` so shared inodes are rewritten
/// once. Symlinks are never followed or rewritten.
fn collect_hardlink_groups(root: &Utf8Path) -> Result<Vec<HardlinkGroup>, PourError> {
    let mut files: Vec<Utf8PathBuf> = Vec::new();
    let mut stack: Vec<Utf8PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(dir.as_std_path()) {
            Ok(entries) => entries,
            // A missing keg subtree is nothing to relocate, not a failure.
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(PourError::io("read_dir", dir.clone(), source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| PourError::io("read_dir", dir.clone(), source))?;
            let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|bad| {
                PourError::InvalidArchive {
                    path: bad.to_string_lossy().into_owned(),
                    reason: "non-UTF-8 keg path".to_owned(),
                }
            })?;
            let meta = fs::symlink_metadata(path.as_std_path())
                .map_err(|source| PourError::io("stat", path.clone(), source))?;
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }

    // Sort so representative selection and report order are deterministic.
    files.sort();

    let mut groups: BTreeMap<(u64, u64), HardlinkGroup> = BTreeMap::new();
    for path in files {
        let meta = fs::symlink_metadata(path.as_std_path())
            .map_err(|source| PourError::io("stat", path.clone(), source))?;
        let key = (meta.dev(), meta.ino());
        match groups.get_mut(&key) {
            Some(group) => group.siblings.push(path),
            None => {
                groups.insert(
                    key,
                    HardlinkGroup {
                        representative: path,
                        siblings: Vec::new(),
                    },
                );
            }
        }
    }

    Ok(groups.into_values().collect())
}

/// Plan the rewrite for one file. Returns `Some((bytes, needs_codesign))` when
/// the file changed, `None` when it is untouched, or an error when the mutation
/// is impossible (rejected before any write).
fn plan_file(
    original: &[u8],
    path: &Utf8Path,
    subs: &[(Vec<u8>, Vec<u8>)],
    new_prefix: &str,
    is_glibc: bool,
    is_gcc: bool,
    ld_so_readable: bool,
) -> Result<Option<(Vec<u8>, bool)>, PourError> {
    if is_text_candidate(path, original) {
        let (rewritten, changed) = replace_all(original, subs);
        return Ok(changed.then_some((rewritten, false)));
    }
    relocate_binary(
        original,
        path,
        subs,
        new_prefix,
        is_glibc,
        is_gcc,
        ld_so_readable,
    )
}

/// Transform a binary file: structured ELF rewrite (runpath/interp, fit-or-grow)
/// or structured Mach-O load-command rewrite first, then the generic NUL-padded
/// placeholder pass, then a mandatory `object` reparse of the mutated bytes.
fn relocate_binary(
    original: &[u8],
    path: &Utf8Path,
    subs: &[(Vec<u8>, Vec<u8>)],
    new_prefix: &str,
    is_glibc: bool,
    is_gcc: bool,
    ld_so_readable: bool,
) -> Result<Option<(Vec<u8>, bool)>, PourError> {
    let format = object_format(original);
    let mut buf = original.to_vec();
    let mut changed = false;
    let mut codesign = false;

    // Structured ELF pass reads the ORIGINAL (placeholder) strings so it knows
    // the true slot capacity; the generic pass below is byte-position preserving
    // so offsets stay valid across both passes.
    let mut protected: Vec<(usize, usize)> = Vec::new();
    if matches!(format, Some(BinaryFormat::Elf)) && !is_glibc {
        if elf_relocate(&mut buf, original, subs, new_prefix, is_gcc, ld_so_readable).map_err(
            |reason| PourError::Relocation {
                path: path.to_path_buf(),
                reason,
            },
        )? {
            changed = true;
        }
        // The dynamic-linkage regions are owned by the structured pass; keep the
        // generic pass off them so stale placeholders left in old slots (grow
        // case) are never re-substituted or falsely rejected.
        protected = elf_linkage_regions(original);
    } else if matches!(format, Some(BinaryFormat::MachO)) || has_thin_macho_magic(original) {
        // Recognized thin Mach-O (even when `object` cannot parse a malformed
        // command table) must take the structured path. Malformed tables return
        // a typed Relocation error before the generic pass or codesign.
        if macho_relocate(&mut buf, original, subs).map_err(|reason| PourError::Relocation {
            path: path.to_path_buf(),
            reason,
        })? {
            changed = true;
        }
        // The load-command string slots are structured-owned.
        protected = macho_linkage_regions(original);
    }
    // Anything appended past the original length (grown strtab/interp) is also
    // structured-owned.
    if buf.len() > original.len() {
        protected.push((original.len(), buf.len()));
    }

    // Generic pass: replace any remaining placeholders in fixed-size NUL slots.
    let (padded, generic_changed) = binary_nul_pad(&buf, subs, &protected, path)?;
    buf = padded;
    if generic_changed {
        changed = true;
    }

    if matches!(format, Some(BinaryFormat::MachO)) || has_thin_macho_magic(original) {
        // Homebrew re-signs relocated Mach-O images; content-detected so the
        // codesign seam is exercised regardless of the host OS.
        codesign = changed;
    }

    if !changed {
        return Ok(None);
    }

    // Mandatory post-mutation reparse; a corrupt result aborts before any write.
    reparse_matches(&buf, format, path)?;
    Ok(Some((buf, codesign)))
}

/// A file is text when it is a libtool archive (`.la`/`.lai`) or has no NUL byte
/// in its first 8 KiB.
fn is_text_candidate(path: &Utf8Path, bytes: &[u8]) -> bool {
    if matches!(path.extension(), Some("la") | Some("lai")) {
        return true;
    }
    let window = bytes.len().min(8192);
    !bytes[..window].contains(&0)
}

/// Placeholder → replacement map from Appendix E, longest key first so no key is
/// matched inside another.
fn placeholder_subs(env: &Env) -> Vec<(Vec<u8>, Vec<u8>)> {
    let prefix = env.prefix.as_str();
    let is_macos = matches!(env.bottle_tag, BottleTag::MacOs { .. });
    let java = if is_macos {
        format!("{prefix}/opt/openjdk/libexec/openjdk.jdk/Contents/Home")
    } else {
        format!("{prefix}/opt/openjdk/libexec")
    };

    let mut subs: Vec<(Vec<u8>, Vec<u8>)> = vec![
        ("@@HOMEBREW_PREFIX@@", env.prefix.as_str().to_owned()),
        ("@@HOMEBREW_CELLAR@@", env.cellar.as_str().to_owned()),
        (
            "@@HOMEBREW_REPOSITORY@@",
            env.repository.as_str().to_owned(),
        ),
        ("@@HOMEBREW_LIBRARY@@", env.library.as_str().to_owned()),
        ("@@HOMEBREW_PERL@@", format!("{prefix}/opt/perl/bin/perl")),
        ("@@HOMEBREW_JAVA@@", java),
    ]
    .into_iter()
    .map(|(key, value)| (key.as_bytes().to_vec(), value.into_bytes()))
    .collect();

    subs.sort_by_key(|item| std::cmp::Reverse(item.0.len()));
    subs
}

/// Replace every placeholder occurrence in `input`; the result may grow.
fn replace_all(input: &[u8], subs: &[(Vec<u8>, Vec<u8>)]) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    let mut changed = false;
    'scan: while index < input.len() {
        for (pattern, replacement) in subs {
            if !pattern.is_empty() && input[index..].starts_with(pattern) {
                out.extend_from_slice(replacement);
                index += pattern.len();
                changed = true;
                continue 'scan;
            }
        }
        out.push(input[index]);
        index += 1;
    }
    (out, changed)
}

/// Generic binary pass: substitute placeholders in fixed-size NUL slots, split
/// on NUL and right-padding each segment back to its original length so total
/// size is unchanged. Byte ranges in `protected` (ELF dynamic-linkage regions
/// and the appended tail, owned by the structured pass) are copied verbatim so
/// their strings are never double-rewritten or falsely rejected for overflow.
fn binary_nul_pad(
    data: &[u8],
    subs: &[(Vec<u8>, Vec<u8>)],
    protected: &[(usize, usize)],
    path: &Utf8Path,
) -> Result<(Vec<u8>, bool), PourError> {
    let mut ranges: Vec<(usize, usize)> = protected
        .iter()
        .map(|&(start, end)| (start.min(data.len()), end.min(data.len())))
        .filter(|&(start, end)| start < end)
        .collect();
    ranges.sort_unstable();

    let mut out = Vec::with_capacity(data.len());
    let mut changed = false;
    let mut cursor = 0usize;
    for &(start, end) in &ranges {
        if start < cursor {
            // Overlapping/adjacent already-consumed range: extend only.
            if end > cursor {
                out.extend_from_slice(&data[cursor..end]);
                cursor = end;
            }
            continue;
        }
        if nul_pad_span(&data[cursor..start], subs, path, &mut out)? {
            changed = true;
        }
        out.extend_from_slice(&data[start..end]);
        cursor = end;
    }
    if nul_pad_span(&data[cursor..], subs, path, &mut out)? {
        changed = true;
    }
    debug_assert_eq!(out.len(), data.len());
    Ok((out, changed))
}

/// NUL-split, size-preserving placeholder substitution of one span, appended to
/// `out`. A replacement that overflows its slot is rejected.
fn nul_pad_span(
    span: &[u8],
    subs: &[(Vec<u8>, Vec<u8>)],
    path: &Utf8Path,
    out: &mut Vec<u8>,
) -> Result<bool, PourError> {
    let mut changed = false;
    for (segment_index, segment) in span.split(|&byte| byte == 0).enumerate() {
        if segment_index > 0 {
            out.push(0);
        }
        let (replaced, segment_changed) = replace_all(segment, subs);
        if replaced.len() > segment.len() {
            return Err(PourError::Relocation {
                path: path.to_path_buf(),
                reason: format!(
                    "relocated string does not fit its {}-byte slot (needs {})",
                    segment.len(),
                    replaced.len()
                ),
            });
        }
        out.extend_from_slice(&replaced);
        out.resize(out.len() + (segment.len() - replaced.len()), 0);
        if segment_changed {
            changed = true;
        }
    }
    Ok(changed)
}

/// Detect a binary's object format, or `None` when `object` cannot parse it.
fn object_format(data: &[u8]) -> Option<BinaryFormat> {
    ObjectFile::parse(data).ok().map(|file| file.format())
}

/// Reparse mutated bytes and require the format to be unchanged.
fn reparse_matches(
    data: &[u8],
    expected: Option<BinaryFormat>,
    path: &Utf8Path,
) -> Result<(), PourError> {
    match (object_format(data), expected) {
        (Some(actual), Some(want)) if actual == want => Ok(()),
        (_, None) => Ok(()),
        _ => Err(PourError::Relocation {
            path: path.to_path_buf(),
            reason: "relocated binary failed to reparse".to_owned(),
        }),
    }
}

fn is_glibc(name: &str) -> bool {
    name == "glibc" || name.starts_with("glibc@")
}

fn is_gcc(name: &str) -> bool {
    name == "gcc" || name.starts_with("gcc@")
}

/// Run `codesign --force --sign - <path>` through the injected runner.
fn codesign(runner: &dyn CommandRunner, path: &Utf8Path) -> Result<(), PourError> {
    let spec = CommandSpec::new("codesign")
        .arg("--force")
        .arg("--sign")
        .arg("-")
        .arg(path.as_str());
    let output = runner
        .run(&spec)
        .map_err(|source| PourError::CommandFailed {
            program: "codesign".to_owned(),
            status: "spawn failed".to_owned(),
            stderr: source.to_string(),
        })?;
    if !output.success() {
        return Err(PourError::CommandFailed {
            program: "codesign".to_owned(),
            status: format!("{}", output.status()),
            stderr: String::from_utf8_lossy(output.stderr()).into_owned(),
        });
    }
    Ok(())
}

/// Monotonic counter making temp names unique within this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Temp-write + fsync + atomic rename, preserving the original file mode.
///
/// The temp file is created with `O_EXCL` (`create_new`) under a unique,
/// non-guessable name in the target directory, so a pre-placed symlink at the
/// temp path cannot redirect the write outside the keg.
fn write_atomic(path: &Utf8Path, bytes: &[u8], mode: u32) -> Result<(), PourError> {
    let file_name = path.file_name().ok_or_else(|| PourError::Relocation {
        path: path.to_path_buf(),
        reason: "keg file has no name".to_owned(),
    })?;
    let parent = path.parent().ok_or_else(|| PourError::Relocation {
        path: path.to_path_buf(),
        reason: "keg file has no parent".to_owned(),
    })?;

    let pid = std::process::id();
    let (temp, mut handle) = loop {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{file_name}.zapbrew-tmp.{pid}.{unique}"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(candidate.as_std_path())
        {
            Ok(handle) => break (candidate, handle),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(PourError::io("create", candidate, source)),
        }
    };

    handle
        .write_all(bytes)
        .map_err(|source| PourError::io("write", temp.clone(), source))?;
    handle
        .sync_all()
        .map_err(|source| PourError::io("sync", temp.clone(), source))?;
    // `mode` at open is masked by umask; set it explicitly on the open handle,
    // never by path, so a swapped directory entry cannot receive the chmod.
    handle
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| PourError::io("chmod", temp.clone(), source))?;
    drop(handle);
    fs::rename(temp.as_std_path(), path.as_std_path())
        .map_err(|source| PourError::io("rename", path.to_path_buf(), source))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// ELF64 structured relocation (little/big endian). 32-bit ELF is left to the
// generic pass. All mutation is capacity-preserving in place, or an append into
// the extended final PT_LOAD for a grown string.
// ---------------------------------------------------------------------------

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: u64 = 0;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;

const EHDR_MIN_LEN: usize = 64;
const PHDR64_LEN: usize = 56;
const DYN64_LEN: usize = 16;
const SHDR64_LEN: usize = 64;

/// Upper bound on a credible `p_align`; larger values would demand a gigantic
/// zero-fill pad before an appended payload and are rejected as malformed.
const MAX_SEGMENT_ALIGN: u64 = 0x1000_0000;

/// One decoded ELF64 program header plus the file offset of the header itself.
#[derive(Clone, Copy)]
struct Phdr64 {
    header_offset: usize,
    p_type: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

/// Perform the ELF64 structured runpath/interp rewrite on `buf`, reading the
/// original placeholder strings from `original`. Returns whether anything
/// changed; `Err(reason)` for an unsupported layout (rejected before any write).
fn elf_relocate(
    buf: &mut Vec<u8>,
    original: &[u8],
    subs: &[(Vec<u8>, Vec<u8>)],
    new_prefix: &str,
    is_gcc: bool,
    ld_so_readable: bool,
) -> Result<bool, String> {
    let Some((little, phdrs)) = decode_elf64(buf) else {
        // Not ELF64 (e.g. ELFCLASS32): generic pass handles it.
        return Ok(false);
    };

    let mut changed = false;

    if rewrite_interp(
        buf,
        original,
        &phdrs,
        little,
        subs,
        new_prefix,
        ld_so_readable,
    )? {
        changed = true;
    }
    if rewrite_runpath(buf, original, &phdrs, little, subs, new_prefix, is_gcc)? {
        changed = true;
    }

    Ok(changed)
}

/// Decode the ELF64 header + program headers. `None` for non-ELF64 or malformed
/// input (handled by the generic pass, never a hard error).
fn decode_elf64(buf: &[u8]) -> Option<(bool, Vec<Phdr64>)> {
    if buf.len() < EHDR_MIN_LEN || &buf[0..4] != b"\x7fELF" || buf[4] != 2 {
        return None;
    }
    let little = match buf[5] {
        1 => true,
        2 => false,
        _ => return None,
    };
    let phoff = rd_u64(buf, 32, little)? as usize;
    let phentsize = rd_u16(buf, 54, little)? as usize;
    let phnum = rd_u16(buf, 56, little)? as usize;
    if phentsize < PHDR64_LEN {
        return None;
    }

    let mut phdrs = Vec::with_capacity(phnum);
    for index in 0..phnum {
        let base = phoff.checked_add(index.checked_mul(phentsize)?)?;
        // The whole header must be addressable without wrapping.
        base.checked_add(PHDR64_LEN)?;
        let p_type = rd_u32(buf, base, little)?;
        let p_offset = rd_u64(buf, base + 8, little)?;
        let p_vaddr = rd_u64(buf, base + 16, little)?;
        let p_filesz = rd_u64(buf, base + 32, little)?;
        let p_memsz = rd_u64(buf, base + 40, little)?;
        let p_align = rd_u64(buf, base + 48, little)?;
        phdrs.push(Phdr64 {
            header_offset: base,
            p_type,
            p_offset,
            p_vaddr,
            p_filesz,
            p_memsz,
            p_align,
        });
    }
    Some((little, phdrs))
}

/// File byte ranges of the ELF dynamic string table and `PT_INTERP` segment —
/// the regions the structured pass owns, kept off-limits to the generic pass.
/// Empty when the input is not decodable ELF64.
fn elf_linkage_regions(data: &[u8]) -> Vec<(usize, usize)> {
    let Some((little, phdrs)) = decode_elf64(data) else {
        return Vec::new();
    };
    let mut regions = Vec::new();
    if let Some(interp) = phdrs.iter().find(|phdr| phdr.p_type == PT_INTERP)
        && let Ok(start) = usize::try_from(interp.p_offset)
        && let Some(end) = interp
            .p_offset
            .checked_add(interp.p_filesz)
            .and_then(|end| usize::try_from(end).ok())
    {
        regions.push((start, end));
    }
    if let Some(dynamic) = phdrs.iter().find(|phdr| phdr.p_type == PT_DYNAMIC) {
        let Ok(dyn_offset) = usize::try_from(dynamic.p_offset) else {
            return regions;
        };
        let count = (dynamic.p_filesz as usize) / DYN64_LEN;
        let mut strtab_vaddr = None;
        let mut strsz = None;
        for index in 0..count {
            let Some(entry) = dyn_offset.checked_add(index * DYN64_LEN) else {
                break;
            };
            let Some(tag) = rd_u64(data, entry, little) else {
                break;
            };
            let value = rd_u64(data, entry + 8, little).unwrap_or(0);
            match tag {
                DT_NULL => break,
                DT_STRTAB => strtab_vaddr = Some(value),
                DT_STRSZ => strsz = Some(value),
                _ => {}
            }
        }
        if let (Some(vaddr), Some(size)) = (strtab_vaddr, strsz)
            && let Some(offset) = vaddr_to_offset(&phdrs, vaddr)
            && let Some(end) = offset.checked_add(size)
            && let (Ok(start), Ok(end)) = (usize::try_from(offset), usize::try_from(end))
        {
            regions.push((start, end));
        }
    }
    regions
}

/// Rewrite `PT_INTERP` to `<prefix>/lib/ld.so` (when readable) or the
/// placeholder-substituted original, in place when it fits, else appended.
fn rewrite_interp(
    buf: &mut Vec<u8>,
    original: &[u8],
    phdrs: &[Phdr64],
    little: bool,
    subs: &[(Vec<u8>, Vec<u8>)],
    new_prefix: &str,
    ld_so_readable: bool,
) -> Result<bool, String> {
    let Some(interp) = phdrs.iter().find(|phdr| phdr.p_type == PT_INTERP) else {
        return Ok(false);
    };
    let offset =
        usize::try_from(interp.p_offset).map_err(|_| "PT_INTERP outside file".to_owned())?;
    let capacity =
        usize::try_from(interp.p_filesz).map_err(|_| "PT_INTERP outside file".to_owned())?;
    let end = offset
        .checked_add(capacity)
        .ok_or_else(|| "PT_INTERP outside file".to_owned())?;
    let original_interp = original
        .get(offset..end)
        .ok_or_else(|| "PT_INTERP outside file".to_owned())?;
    let current = cstr(original_interp);

    let desired: Vec<u8> = if ld_so_readable {
        format!("{new_prefix}/lib/ld.so").into_bytes()
    } else {
        replace_all(current, subs).0
    };

    if desired.as_slice() == current {
        return Ok(false);
    }

    if desired.len() < capacity {
        // Fits with room for a NUL terminator: overwrite in place.
        zero_range(buf, offset, capacity)?;
        write_bytes(buf, offset, &desired)?;
        return Ok(true);
    }

    // Grown interpreter path: append into the extended final PT_LOAD.
    let mut payload = desired.clone();
    payload.push(0);
    let (file_offset, vaddr) = append_into_last_load(buf, &payload, little)?;
    let header = interp.header_offset;
    write_u64(buf, header + 8, little, file_offset)?; // p_offset
    write_u64(buf, header + 16, little, vaddr)?; // p_vaddr
    write_u64(buf, header + 24, little, vaddr)?; // p_paddr
    write_u64(buf, header + 32, little, desired.len() as u64 + 1)?; // p_filesz
    write_u64(buf, header + 40, little, desired.len() as u64 + 1)?; // p_memsz
    Ok(true)
}

/// Rewrite `DT_RUNPATH` (preferred) or `DT_RPATH`: keep `$ORIGIN`-relative and
/// new-prefix segments, map `lib/gcc/<n>` → `lib/gcc/current` (non-gcc), ensure
/// `<prefix>/lib` is present. Fits in place, else appends a copied string table.
fn rewrite_runpath(
    buf: &mut Vec<u8>,
    original: &[u8],
    phdrs: &[Phdr64],
    little: bool,
    subs: &[(Vec<u8>, Vec<u8>)],
    new_prefix: &str,
    is_gcc: bool,
) -> Result<bool, String> {
    let Some(dynamic) = phdrs.iter().find(|phdr| phdr.p_type == PT_DYNAMIC) else {
        return Ok(false);
    };
    let dyn_offset =
        usize::try_from(dynamic.p_offset).map_err(|_| "PT_DYNAMIC outside file".to_owned())?;
    let count = (dynamic.p_filesz as usize) / DYN64_LEN;

    let mut strtab_vaddr: Option<u64> = None;
    let mut strtab_value_offset: Option<usize> = None;
    let mut strsz: Option<u64> = None;
    let mut strsz_value_offset: Option<usize> = None;
    let mut runpath_value: Option<u64> = None;
    let mut runpath_value_offset: Option<usize> = None;
    let mut runpath_tag: Option<u64> = None;

    for index in 0..count {
        let entry = dyn_offset
            .checked_add(index * DYN64_LEN)
            .ok_or_else(|| "dynamic entry truncated".to_owned())?;
        let tag = rd_u64(buf, entry, little).ok_or_else(|| "dynamic entry truncated".to_owned())?;
        let value_offset = entry + 8;
        match tag {
            DT_NULL => break,
            DT_STRTAB => {
                strtab_vaddr = rd_u64(buf, value_offset, little);
                strtab_value_offset = Some(value_offset);
            }
            DT_STRSZ => {
                strsz = rd_u64(buf, value_offset, little);
                strsz_value_offset = Some(value_offset);
            }
            DT_RUNPATH => {
                // DT_RUNPATH always wins over an earlier DT_RPATH.
                runpath_value = rd_u64(buf, value_offset, little);
                runpath_value_offset = Some(value_offset);
                runpath_tag = Some(DT_RUNPATH);
            }
            DT_RPATH if runpath_tag != Some(DT_RUNPATH) => {
                runpath_value = rd_u64(buf, value_offset, little);
                runpath_value_offset = Some(value_offset);
                runpath_tag = Some(DT_RPATH);
            }
            _ => {}
        }
    }

    let (Some(runpath_value), Some(runpath_value_offset)) = (runpath_value, runpath_value_offset)
    else {
        return Ok(false);
    };
    let Some(strtab_vaddr) = strtab_vaddr else {
        return Ok(false);
    };
    // Map the string-table vaddr to a file offset; if it is unmapped we cannot
    // do the structured rewrite (the generic pass already substituted it).
    let Some(strtab_file) = vaddr_to_offset(phdrs, strtab_vaddr) else {
        return Ok(false);
    };
    // The runpath span must lie inside the mapped string table, which must lie
    // inside the file; anything else is a malformed image, rejected unwritten.
    let strsz = strsz.ok_or_else(|| "DT_STRSZ missing".to_owned())?;
    let table_start =
        usize::try_from(strtab_file).map_err(|_| "string table outside file".to_owned())?;
    let table_end = strtab_file
        .checked_add(strsz)
        .and_then(|end| usize::try_from(end).ok())
        .ok_or_else(|| "string table span overflows".to_owned())?;
    if table_end > original.len() {
        return Err("string table outside file".to_owned());
    }
    if runpath_value >= strsz {
        return Err("DT_RUNPATH offset outside string table".to_owned());
    }
    // runpath_value < strsz, so this stays within table_end (checked above).
    let runpath_file = table_start + runpath_value as usize;
    let table_tail = &original[runpath_file..table_end];
    let nul = table_tail
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| "DT_RUNPATH not NUL-terminated inside string table".to_owned())?;
    let original_runpath = &table_tail[..nul];
    let capacity = original_runpath.len();

    let substituted = replace_all(original_runpath, subs).0;
    let substituted = String::from_utf8_lossy(&substituted).into_owned();
    let rewritten = compute_runpath(&substituted, new_prefix, is_gcc);
    let rewritten_bytes = rewritten.as_bytes();

    if rewritten_bytes == original_runpath {
        return Ok(false);
    }

    if rewritten_bytes.len() <= capacity {
        // In place: zero the old slot (string + terminator) then write.
        zero_range(buf, runpath_file, capacity + 1)?;
        write_bytes(buf, runpath_file, rewritten_bytes)?;
        return Ok(true);
    }

    // Grow: append a copied string table with the longer runpath appended, and
    // repoint DT_STRTAB/DT_STRSZ/DT_RUNPATH (and the .dynstr section header).
    let strsz_value_offset =
        strsz_value_offset.ok_or_else(|| "DT_STRSZ offset missing".to_owned())?;
    let strtab_value_offset =
        strtab_value_offset.ok_or_else(|| "DT_STRTAB offset missing".to_owned())?;

    let old_table = buf
        .get(table_start..table_end)
        .ok_or_else(|| "string table outside file".to_owned())?
        .to_vec();
    let new_runpath_offset = old_table.len() as u64;
    let mut new_table = old_table;
    new_table.extend_from_slice(rewritten_bytes);
    new_table.push(0);
    let new_strsz = new_table.len() as u64;

    let (table_file, table_vaddr) = append_into_last_load(buf, &new_table, little)?;

    write_u64(buf, strtab_value_offset, little, table_vaddr)?;
    write_u64(buf, strsz_value_offset, little, new_strsz)?;
    write_u64(buf, runpath_value_offset, little, new_runpath_offset)?;
    update_dynstr_section(
        buf,
        little,
        strtab_vaddr,
        table_file,
        table_vaddr,
        new_strsz,
    );
    Ok(true)
}

/// Build the relocated runpath: keep only `$ORIGIN`-relative or new-prefix
/// segments, rewrite `lib/gcc/<n>` → `lib/gcc/current` (unless this is gcc),
/// and guarantee `<prefix>/lib` is present.
fn compute_runpath(current: &str, new_prefix: &str, is_gcc: bool) -> String {
    let lib_dir = format!("{new_prefix}/lib");
    let mut segments: Vec<String> = Vec::new();
    for raw in current.split(':') {
        if raw.is_empty() {
            continue;
        }
        if !(raw.starts_with(new_prefix) || raw.starts_with("$ORIGIN")) {
            continue;
        }
        let segment = if is_gcc {
            raw.to_owned()
        } else {
            rewrite_gcc_lib(raw)
        };
        if !segments.contains(&segment) {
            segments.push(segment);
        }
    }
    if !segments.iter().any(|segment| segment == &lib_dir) {
        segments.push(lib_dir);
    }
    segments.join(":")
}

/// Replace a trailing `/lib/gcc/<digits>` component with `/lib/gcc/current`.
fn rewrite_gcc_lib(segment: &str) -> String {
    const MARKER: &str = "/lib/gcc/";
    if let Some(position) = segment.rfind(MARKER) {
        let tail = &segment[position + MARKER.len()..];
        if !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit()) {
            return format!("{}{MARKER}current", &segment[..position]);
        }
    }
    segment.to_owned()
}

/// Append `payload` at the end of `buf` inside the extended final `PT_LOAD`
/// (the loadable segment with the highest vaddr), returning the appended data's
/// file offset and virtual address. Re-reads the segment so repeated appends
/// stay consistent.
fn append_into_last_load(
    buf: &mut Vec<u8>,
    payload: &[u8],
    little: bool,
) -> Result<(u64, u64), String> {
    let (_, phdrs) =
        decode_elf64(buf).ok_or_else(|| "cannot re-decode ELF for append".to_owned())?;
    let last = phdrs
        .iter()
        .filter(|phdr| phdr.p_type == PT_LOAD)
        .max_by_key(|phdr| phdr.p_vaddr)
        .ok_or_else(|| "no PT_LOAD segment to extend".to_owned())?;

    // Growing is only safe when the segment carries no BSS and its file extent
    // is exactly the current end of file: raising p_filesz over a
    // p_memsz > p_filesz tail would overwrite bytes the loader must zero-fill,
    // and extending a non-terminal extent would fold later file bytes into
    // this mapping.
    if last.p_memsz != last.p_filesz {
        return Err("final PT_LOAD has BSS (p_memsz != p_filesz), cannot extend".to_owned());
    }
    let extent_end = last
        .p_offset
        .checked_add(last.p_filesz)
        .ok_or_else(|| "final PT_LOAD extent overflows".to_owned())?;
    if extent_end != buf.len() as u64 {
        return Err("final PT_LOAD is not the terminal file extent".to_owned());
    }

    let align = if last.p_align > 1 { last.p_align } else { 16 };
    if align > MAX_SEGMENT_ALIGN {
        return Err("unreasonable final PT_LOAD alignment".to_owned());
    }
    let file_offset = round_up(buf.len() as u64, align)
        .ok_or_else(|| "aligned append offset overflows".to_owned())?;
    while (buf.len() as u64) < file_offset {
        buf.push(0);
    }
    // vaddr - offset is invariant for the segment, so congruence mod p_align is
    // preserved for any appended file offset.
    let delta = file_offset
        .checked_sub(last.p_offset)
        .ok_or_else(|| "append offset precedes final PT_LOAD".to_owned())?;
    let vaddr = last
        .p_vaddr
        .checked_add(delta)
        .ok_or_else(|| "appended vaddr overflows".to_owned())?;
    buf.extend_from_slice(payload);

    let new_size = (buf.len() as u64)
        .checked_sub(last.p_offset)
        .ok_or_else(|| "extended PT_LOAD size overflows".to_owned())?;
    write_u64(buf, last.header_offset + 32, little, new_size)?; // p_filesz
    write_u64(buf, last.header_offset + 40, little, new_size)?; // p_memsz
    Ok((file_offset, vaddr))
}

/// Repoint the `.dynstr` section header (sh_addr matching the old string-table
/// vaddr) to the appended copy, keeping section-based tools consistent.
fn update_dynstr_section(
    buf: &mut [u8],
    little: bool,
    old_vaddr: u64,
    new_offset: u64,
    new_vaddr: u64,
    new_size: u64,
) {
    let Some(shoff) = rd_u64(buf, 40, little) else {
        return;
    };
    if shoff == 0 {
        return;
    }
    let shentsize = rd_u16(buf, 58, little).unwrap_or(0) as usize;
    let shnum = rd_u16(buf, 60, little).unwrap_or(0) as usize;
    if shentsize < SHDR64_LEN {
        return;
    }
    for index in 0..shnum {
        let base = shoff as usize + index * shentsize;
        if rd_u64(buf, base + 16, little) == Some(old_vaddr) {
            let _ = write_u64(buf, base + 16, little, new_vaddr); // sh_addr
            let _ = write_u64(buf, base + 24, little, new_offset); // sh_offset
            let _ = write_u64(buf, base + 32, little, new_size); // sh_size
        }
    }
}

/// Map a virtual address to a file offset via the covering PT_LOAD segment.
fn vaddr_to_offset(phdrs: &[Phdr64], vaddr: u64) -> Option<u64> {
    phdrs
        .iter()
        .filter(|phdr| phdr.p_type == PT_LOAD)
        .find_map(|phdr| {
            let end = phdr.p_vaddr.checked_add(phdr.p_filesz)?;
            if vaddr >= phdr.p_vaddr && vaddr < end {
                phdr.p_offset.checked_add(vaddr - phdr.p_vaddr)
            } else {
                None
            }
        })
}

/// Bytes up to the first NUL.
fn cstr(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&byte| byte == 0) {
        Some(end) => &bytes[..end],
        None => bytes,
    }
}

/// Round `value` up to a multiple of `align`; `None` on overflow.
fn round_up(value: u64, align: u64) -> Option<u64> {
    if align == 0 {
        return Some(value);
    }
    value.div_ceil(align).checked_mul(align)
}

fn zero_range(buf: &mut [u8], offset: usize, len: usize) -> Result<(), String> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| "write range outside file".to_owned())?;
    let slice = buf
        .get_mut(offset..end)
        .ok_or_else(|| "write range outside file".to_owned())?;
    slice.fill(0);
    Ok(())
}

fn write_bytes(buf: &mut [u8], offset: usize, bytes: &[u8]) -> Result<(), String> {
    let end = offset
        .checked_add(bytes.len())
        .ok_or_else(|| "write range outside file".to_owned())?;
    let slice = buf
        .get_mut(offset..end)
        .ok_or_else(|| "write range outside file".to_owned())?;
    slice.copy_from_slice(bytes);
    Ok(())
}

fn rd_u16(buf: &[u8], offset: usize, little: bool) -> Option<u16> {
    let bytes = buf.get(offset..offset.checked_add(2)?)?;
    let array = [bytes[0], bytes[1]];
    Some(if little {
        u16::from_le_bytes(array)
    } else {
        u16::from_be_bytes(array)
    })
}

fn rd_u32(buf: &[u8], offset: usize, little: bool) -> Option<u32> {
    let bytes = buf.get(offset..offset.checked_add(4)?)?;
    let array = [bytes[0], bytes[1], bytes[2], bytes[3]];
    Some(if little {
        u32::from_le_bytes(array)
    } else {
        u32::from_be_bytes(array)
    })
}

fn rd_u64(buf: &[u8], offset: usize, little: bool) -> Option<u64> {
    let bytes = buf.get(offset..offset.checked_add(8)?)?;
    let mut array = [0u8; 8];
    array.copy_from_slice(bytes);
    Some(if little {
        u64::from_le_bytes(array)
    } else {
        u64::from_be_bytes(array)
    })
}

fn write_u64(buf: &mut [u8], offset: usize, little: bool, value: u64) -> Result<(), String> {
    let encoded = if little {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    write_bytes(buf, offset, &encoded)
}

// ---------------------------------------------------------------------------
// Mach-O structured relocation. `LC_ID_DYLIB`/`LC_LOAD_DYLIB`/`LC_RPATH`
// strings are rewritten in place; the slot capacity is everything from the
// string's start to the end of its load command (`cmdsize`), so a replacement
// may grow into the command's trailing NUL padding. 32- and 64-bit thin images
// (either endianness). Universal/fat binaries are not thin Mach-O and skip this
// pass. A recognized thin image with a malformed/unsupported command table
// returns a typed error before the generic size-preserving pass or codesign;
// only non-Mach-O (and non-thin) inputs decline into that generic pass.
// ---------------------------------------------------------------------------

const MH_MAGIC: u32 = 0xfeed_face;
/// `MH_MAGIC` as seen through an opposite-endian read.
const MH_CIGAM: u32 = 0xcefa_edfe;
const MH_MAGIC_64: u32 = 0xfeed_facf;
/// `MH_MAGIC_64` as seen through an opposite-endian read.
const MH_CIGAM_64: u32 = 0xcffa_edfe;
const MACHO_HEADER32_LEN: usize = 28;
const MACHO_HEADER64_LEN: usize = 32;
const LC_LOAD_DYLIB: u32 = 0xc;
const LC_ID_DYLIB: u32 = 0xd;
const LC_RPATH: u32 = 0x8000_001c;
/// Smallest `lc_str` offset: an `rpath_command` is 12 bytes before its string.
const MIN_LC_STR_OFFSET: usize = 12;

/// One rewritable load-command string slot: from the string's start to the end
/// of its command.
struct MachSlot {
    start: usize,
    end: usize,
}

/// True when `data` begins with a thin Mach-O magic (32/64-bit, either endian).
fn has_thin_macho_magic(data: &[u8]) -> bool {
    matches!(
        rd_u32(data, 0, true),
        Some(MH_MAGIC | MH_CIGAM | MH_MAGIC_64 | MH_CIGAM_64)
    )
}

/// Decode thin Mach-O header endianness and header length, or `None` when the
/// input is not thin Mach-O.
fn thin_macho_header(data: &[u8]) -> Option<(bool, usize)> {
    match rd_u32(data, 0, true)? {
        MH_MAGIC => Some((true, MACHO_HEADER32_LEN)),
        MH_CIGAM => Some((false, MACHO_HEADER32_LEN)),
        MH_MAGIC_64 => Some((true, MACHO_HEADER64_LEN)),
        MH_CIGAM_64 => Some((false, MACHO_HEADER64_LEN)),
        _ => None,
    }
}

/// Decode the rewritable load-command string slots of a thin Mach-O image.
///
/// - `Ok(None)` when the input is not thin Mach-O (caller may use the generic pass).
/// - `Ok(Some(slots))` when the command table is well-formed.
/// - `Err` when thin Mach-O magic is recognized but the command table is
///   malformed or unsupported (typed rejection; no generic fallback).
fn macho_string_slots(data: &[u8]) -> Result<Option<Vec<MachSlot>>, String> {
    let Some((little, header_len)) = thin_macho_header(data) else {
        return Ok(None);
    };
    let ncmds = rd_u32(data, 16, little)
        .ok_or_else(|| "malformed Mach-O load command table".to_owned())? as usize;
    let sizeofcmds = rd_u32(data, 20, little)
        .ok_or_else(|| "malformed Mach-O load command table".to_owned())?
        as usize;
    let cmds_end = header_len
        .checked_add(sizeofcmds)
        .ok_or_else(|| "malformed Mach-O load command table".to_owned())?;
    if cmds_end > data.len() {
        return Err("malformed Mach-O load command table".to_owned());
    }

    let mut slots = Vec::new();
    let mut cursor = header_len;
    for _ in 0..ncmds {
        let cmd = rd_u32(data, cursor, little)
            .ok_or_else(|| "malformed Mach-O load command table".to_owned())?;
        let cmdsize = rd_u32(
            data,
            cursor
                .checked_add(4)
                .ok_or_else(|| "malformed Mach-O load command table".to_owned())?,
            little,
        )
        .ok_or_else(|| "malformed Mach-O load command table".to_owned())?
            as usize;
        let cmd_end = cursor
            .checked_add(cmdsize)
            .ok_or_else(|| "malformed Mach-O load command table".to_owned())?;
        if cmdsize < 8 || cmd_end > cmds_end {
            return Err("malformed Mach-O load command table".to_owned());
        }
        if matches!(cmd, LC_ID_DYLIB | LC_LOAD_DYLIB | LC_RPATH) {
            let str_off = rd_u32(
                data,
                cursor
                    .checked_add(8)
                    .ok_or_else(|| "malformed Mach-O load command table".to_owned())?,
                little,
            )
            .ok_or_else(|| "malformed Mach-O load command table".to_owned())?
                as usize;
            let start = cursor
                .checked_add(str_off)
                .ok_or_else(|| "malformed Mach-O load command table".to_owned())?;
            if str_off < MIN_LC_STR_OFFSET || start >= cmd_end {
                return Err("malformed Mach-O load command table".to_owned());
            }
            slots.push(MachSlot {
                start,
                end: cmd_end,
            });
        }
        cursor = cmd_end;
    }
    Ok(Some(slots))
}

/// Rewrite the dylib/rpath load-command strings of `buf` in place, reading the
/// original strings (and slot capacities) from `original`. `Ok(false)` when the
/// input is not thin Mach-O. `Err` when a recognized thin image has a malformed
/// command table, or when a replacement cannot fit its command slot.
fn macho_relocate(
    buf: &mut [u8],
    original: &[u8],
    subs: &[(Vec<u8>, Vec<u8>)],
) -> Result<bool, String> {
    let Some(slots) = macho_string_slots(original)? else {
        return Ok(false);
    };
    let mut changed = false;
    for slot in slots {
        let slot_bytes = original
            .get(slot.start..slot.end)
            .ok_or_else(|| "load command outside file".to_owned())?;
        let current = cstr(slot_bytes);
        let (replaced, slot_changed) = replace_all(current, subs);
        if !slot_changed {
            continue;
        }
        let capacity = slot_bytes.len();
        // The replacement plus its NUL terminator must fit the command slot;
        // the trailing padding inside cmdsize is legitimate capacity.
        if replaced.len() >= capacity {
            return Err(format!(
                "relocated load-command string needs {} bytes plus NUL but its \
                 command slot holds {capacity}",
                replaced.len()
            ));
        }
        zero_range(buf, slot.start, capacity)?;
        write_bytes(buf, slot.start, &replaced)?;
        changed = true;
    }
    Ok(changed)
}

/// Load-command string slots as protected byte ranges for the generic pass.
fn macho_linkage_regions(data: &[u8]) -> Vec<(usize, usize)> {
    match macho_string_slots(data) {
        Ok(Some(slots)) => slots
            .into_iter()
            .map(|slot| (slot.start, slot.end))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::Mutex;

    use object::write::{Object as WriteObject, StandardSection};
    use object::{Architecture, Endianness};
    use zapbrew_prefix::CommandOutput;

    // `unwrap`/`expect` are denied workspace-wide; these panic on the failure
    // path so assertions stay meaningful.
    fn some<T>(value: Option<T>, message: &str) -> T {
        match value {
            Some(inner) => inner,
            None => panic!("{message}"),
        }
    }
    fn okr<T, E: std::fmt::Debug>(value: Result<T, E>, message: &str) -> T {
        match value {
            Ok(inner) => inner,
            Err(error) => panic!("{message}: {error:?}"),
        }
    }

    // ---- byte helpers ----

    fn wr16(buf: &mut [u8], offset: usize, value: u16) {
        buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn wr32(buf: &mut [u8], offset: usize, value: u32) {
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn wr64(buf: &mut [u8], offset: usize, value: u64) {
        buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn subs_for(prefix: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
        vec![(b"@@HOMEBREW_PREFIX@@".to_vec(), prefix.as_bytes().to_vec())]
    }

    /// Hand-assemble a minimal ELF64 LE x86-64 executable with one PT_LOAD
    /// covering the whole file, a PT_INTERP, and a PT_DYNAMIC pointing at a
    /// dynamic string table containing a DT_RUNPATH. `object::write` cannot
    /// emit executables with program headers, so the fixture is built by hand;
    /// `object` then parses it, which is the relevant validation.
    fn build_elf64(interp: &str, runpath: &str) -> Vec<u8> {
        const BASE: u64 = 0x40_0000;
        let phoff = EHDR_MIN_LEN; // 64
        let phnum = 3usize;
        let ph_table_len = phnum * PHDR64_LEN; // 168
        let data_start = phoff + ph_table_len; // 232

        let interp_off = data_start;
        let mut interp_bytes = interp.as_bytes().to_vec();
        interp_bytes.push(0);
        let dynstr_off = interp_off + interp_bytes.len();
        // dynstr: index 0 is the empty string, runpath at offset 1.
        let runpath_str_off = 1u64;
        let mut dynstr = vec![0u8];
        dynstr.extend_from_slice(runpath.as_bytes());
        dynstr.push(0);
        let dyn_off = dynstr_off + dynstr.len();
        let dyn_entries: [(u64, u64); 4] = [
            (DT_STRTAB, BASE + dynstr_off as u64),
            (DT_STRSZ, dynstr.len() as u64),
            (DT_RUNPATH, runpath_str_off),
            (DT_NULL, 0),
        ];
        let dyn_len = dyn_entries.len() * DYN64_LEN;
        let total = dyn_off + dyn_len;

        let mut buf = vec![0u8; total];
        buf[0..4].copy_from_slice(b"\x7fELF");
        buf[4] = 2; // ELFCLASS64
        buf[5] = 1; // ELFDATA2LSB
        buf[6] = 1; // EV_CURRENT
        wr16(&mut buf, 16, 2); // e_type = ET_EXEC
        wr16(&mut buf, 18, 62); // e_machine = EM_X86_64
        wr32(&mut buf, 20, 1); // e_version
        wr64(&mut buf, 24, BASE); // e_entry
        wr64(&mut buf, 32, phoff as u64); // e_phoff
        wr64(&mut buf, 40, 0); // e_shoff (no sections)
        wr32(&mut buf, 48, 0); // e_flags
        wr16(&mut buf, 52, EHDR_MIN_LEN as u16); // e_ehsize
        wr16(&mut buf, 54, PHDR64_LEN as u16); // e_phentsize
        wr16(&mut buf, 56, phnum as u16); // e_phnum
        wr16(&mut buf, 58, 0); // e_shentsize
        wr16(&mut buf, 60, 0); // e_shnum
        wr16(&mut buf, 62, 0); // e_shstrndx

        let write_phdr = |buf: &mut [u8],
                          idx: usize,
                          ptype: u32,
                          flags: u32,
                          off: u64,
                          filesz: u64,
                          align: u64| {
            let base = phoff + idx * PHDR64_LEN;
            wr32(buf, base, ptype);
            wr32(buf, base + 4, flags);
            wr64(buf, base + 8, off);
            wr64(buf, base + 16, BASE + off);
            wr64(buf, base + 24, BASE + off);
            wr64(buf, base + 32, filesz);
            wr64(buf, base + 40, filesz);
            wr64(buf, base + 48, align);
        };
        write_phdr(&mut buf, 0, PT_LOAD, 5, 0, total as u64, 0x1000);
        write_phdr(
            &mut buf,
            1,
            PT_DYNAMIC,
            6,
            dyn_off as u64,
            dyn_len as u64,
            8,
        );
        write_phdr(
            &mut buf,
            2,
            PT_INTERP,
            4,
            interp_off as u64,
            interp_bytes.len() as u64,
            1,
        );

        buf[interp_off..interp_off + interp_bytes.len()].copy_from_slice(&interp_bytes);
        buf[dynstr_off..dynstr_off + dynstr.len()].copy_from_slice(&dynstr);
        for (idx, (tag, value)) in dyn_entries.iter().enumerate() {
            let base = dyn_off + idx * DYN64_LEN;
            wr64(&mut buf, base, *tag);
            wr64(&mut buf, base + 8, *value);
        }
        buf
    }

    fn build_macho() -> Vec<u8> {
        let mut object = WriteObject::new(
            BinaryFormat::MachO,
            Architecture::X86_64,
            Endianness::Little,
        );
        let section = object.section_id(StandardSection::Data);
        object.append_section_data(section, b"@@HOMEBREW_PREFIX@@/lib/x\x00", 8);
        okr(object.write(), "write macho")
    }

    /// Re-decode DT_RUNPATH from a relocated ELF, independent of the mutation
    /// paths so the assertion is meaningful.
    fn read_runpath(buf: &[u8]) -> String {
        let (little, phdrs) = some(decode_elf64(buf), "elf64");
        let dynamic = some(phdrs.iter().find(|p| p.p_type == PT_DYNAMIC), "dynamic");
        let dyn_off = dynamic.p_offset as usize;
        let count = (dynamic.p_filesz as usize) / DYN64_LEN;
        let mut strtab = None;
        let mut runpath = None;
        for i in 0..count {
            let entry = dyn_off + i * DYN64_LEN;
            let tag = some(rd_u64(buf, entry, little), "tag");
            let val = some(rd_u64(buf, entry + 8, little), "val");
            match tag {
                DT_NULL => break,
                DT_STRTAB => strtab = Some(val),
                DT_RUNPATH | DT_RPATH => runpath = Some(val),
                _ => {}
            }
        }
        let strtab_file = some(vaddr_to_offset(&phdrs, some(strtab, "strtab")), "map");
        let at = (strtab_file + some(runpath, "runpath")) as usize;
        String::from_utf8_lossy(cstr(&buf[at..])).into_owned()
    }

    fn read_interp(buf: &[u8]) -> String {
        let (_, phdrs) = some(decode_elf64(buf), "elf64");
        let interp = some(phdrs.iter().find(|p| p.p_type == PT_INTERP), "interp");
        let at = interp.p_offset as usize;
        let cap = interp.p_filesz as usize;
        String::from_utf8_lossy(cstr(&buf[at..at + cap])).into_owned()
    }

    fn relocate_bytes(
        original: &[u8],
        prefix: &str,
        ld_readable: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, PourError> {
        relocate_binary(
            original,
            Utf8Path::new("keg/file"),
            &subs_for(prefix),
            prefix,
            false,
            false,
            ld_readable,
        )
    }

    struct RecordingRunner {
        calls: Mutex<Vec<Vec<String>>>,
        fail: bool,
    }

    impl RecordingRunner {
        fn new(fail: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail,
            }
        }
        fn recorded(&self) -> Vec<Vec<String>> {
            match self.calls.lock() {
                Ok(guard) => guard.clone(),
                Err(_) => panic!("recording runner lock poisoned"),
            }
        }
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
            let mut argv = vec![spec.program().to_string_lossy().into_owned()];
            for arg in spec.arguments() {
                argv.push(arg.to_string_lossy().into_owned());
            }
            match self.calls.lock() {
                Ok(mut guard) => guard.push(argv),
                Err(_) => return Err(std::io::Error::other("poisoned")),
            }
            let code = if self.fail { 1 } else { 0 };
            Ok(CommandOutput::new(
                ExitStatus::from_raw(code << 8),
                Vec::new(),
                b"boom".to_vec(),
            ))
        }
    }

    #[test]
    fn text_pass_substitutes_placeholders() {
        let subs = subs_for("/opt/hb");
        let (out, changed) = replace_all(b"PATH=@@HOMEBREW_PREFIX@@/bin\n", &subs);
        assert!(changed);
        assert_eq!(out, b"PATH=/opt/hb/bin\n");
    }

    #[test]
    fn nul_pad_pass_preserves_size_and_rejects_growth() {
        let subs = subs_for("/opt/hb");
        let data = b"\x00@@HOMEBREW_PREFIX@@/lib/x\x00tail\x00";
        let (out, changed) = okr(binary_nul_pad(data, &subs, &[], Utf8Path::new("f")), "fits");
        assert!(changed);
        assert_eq!(out.len(), data.len());
        assert!(out.windows(11).any(|w| w == b"/opt/hb/lib"));

        let long = subs_for("/a/very/long/custom/homebrew/prefix/location");
        let err = binary_nul_pad(data, &long, &[], Utf8Path::new("f"));
        assert!(matches!(err, Err(PourError::Relocation { .. })));

        // A protected range is copied verbatim, so an oversized placeholder
        // inside it is neither rewritten nor rejected.
        let protected = binary_nul_pad(data, &long, &[(1, 26)], Utf8Path::new("f"));
        let (out, _) = okr(protected, "protected fits");
        assert_eq!(&out[1..26], &data[1..26]);
    }

    #[test]
    fn elf_runpath_fits_in_place() {
        let elf = build_elf64(
            "@@HOMEBREW_PREFIX@@/lib/ld.so",
            "@@HOMEBREW_PREFIX@@/lib/foo:$ORIGIN/../lib",
        );
        let original_len = elf.len();
        let (relocated, _) = some(
            okr(relocate_bytes(&elf, "/opt/hb", true), "plan"),
            "changed",
        );
        assert_eq!(relocated.len(), original_len);
        assert_eq!(object_format(&relocated), Some(BinaryFormat::Elf));
        assert_eq!(
            read_runpath(&relocated),
            "/opt/hb/lib/foo:$ORIGIN/../lib:/opt/hb/lib"
        );
        assert_eq!(read_interp(&relocated), "/opt/hb/lib/ld.so");
    }

    #[test]
    fn elf_runpath_grows_into_extended_load() {
        let elf = build_elf64(
            "@@HOMEBREW_PREFIX@@/lib/ld.so",
            "@@HOMEBREW_PREFIX@@/lib/foo:$ORIGIN/../lib",
        );
        let original_len = elf.len();
        let prefix = "/a/very/long/custom/homebrew/prefix/location/deep";
        let (relocated, _) = some(okr(relocate_bytes(&elf, prefix, false), "plan"), "changed");
        assert!(relocated.len() > original_len);
        assert_eq!(object_format(&relocated), Some(BinaryFormat::Elf));
        let expected = format!("{prefix}/lib/foo:$ORIGIN/../lib:{prefix}/lib");
        assert_eq!(read_runpath(&relocated), expected);
    }

    #[test]
    fn elf_grow_without_loadable_segment_is_rejected_unchanged() {
        let mut elf = build_elf64(
            "@@HOMEBREW_PREFIX@@/lib/ld.so",
            "@@HOMEBREW_PREFIX@@/lib/foo",
        );
        let phoff = EHDR_MIN_LEN;
        wr32(&mut elf, phoff, 0); // PT_LOAD -> PT_NULL
        let before = elf.clone();
        let prefix = "/a/very/long/custom/homebrew/prefix/location/deep";
        let result = relocate_bytes(&elf, prefix, false);
        assert!(matches!(result, Err(PourError::Relocation { .. })));
        assert_eq!(elf, before);
    }

    #[test]
    fn macho_relocation_flags_codesign_and_reparses() {
        let macho = build_macho();
        assert_eq!(object_format(&macho), Some(BinaryFormat::MachO));
        let (bytes, codesign) = some(
            okr(relocate_bytes(&macho, "/opt/hb", false), "plan"),
            "changed",
        );
        assert!(codesign, "modified Mach-O must be re-signed");
        assert_eq!(object_format(&bytes), Some(BinaryFormat::MachO));
    }

    #[test]
    fn codesign_issues_expected_command() {
        let runner = RecordingRunner::new(false);
        okr(
            codesign(&runner, Utf8Path::new("/keg/bin/tool")),
            "codesign",
        );
        let calls = runner.recorded();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            vec!["codesign", "--force", "--sign", "-", "/keg/bin/tool"]
        );
    }

    #[test]
    fn codesign_failure_surfaces_command_error() {
        let runner = RecordingRunner::new(true);
        let result = codesign(&runner, Utf8Path::new("/keg/bin/tool"));
        assert!(matches!(result, Err(PourError::CommandFailed { .. })));
    }

    #[test]
    fn skip_relocation_constant_matches_bottle_value() {
        assert_eq!(SKIP_RELOCATION, ":any_skip_relocation");
    }

    #[test]
    fn gcc_lib_version_folds_to_current() {
        assert_eq!(
            rewrite_gcc_lib("/opt/hb/lib/gcc/13"),
            "/opt/hb/lib/gcc/current"
        );
        assert_eq!(
            rewrite_gcc_lib("/opt/hb/lib/gcc/current"),
            "/opt/hb/lib/gcc/current"
        );
        assert_eq!(rewrite_gcc_lib("/opt/hb/lib"), "/opt/hb/lib");
    }

    #[test]
    fn runpath_drops_foreign_segments_and_ensures_lib() {
        let out = compute_runpath("/opt/hb/lib/foo:/usr/lib:$ORIGIN/../lib", "/opt/hb", false);
        assert_eq!(out, "/opt/hb/lib/foo:$ORIGIN/../lib:/opt/hb/lib");
    }

    // ---- Task 5 relocation-hardening regressions ----

    #[test]
    fn write_atomic_preserves_exact_mode() {
        let temp = okr(tempfile::TempDir::new(), "temp");
        let root = some(
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).ok(),
            "utf8 temp",
        );
        let target = root.join("bin/tool");
        okr(
            fs::create_dir_all(some(target.parent(), "parent").as_std_path()),
            "mkdir",
        );
        okr(fs::write(target.as_std_path(), b"old"), "seed");
        okr(write_atomic(&target, b"new", 0o755), "write_atomic");
        let meta = okr(fs::metadata(target.as_std_path()), "stat");
        assert_eq!(meta.mode() & 0o7777, 0o755, "mode must survive umask");
        assert_eq!(okr(fs::read(target.as_std_path()), "read"), b"new");
    }

    /// File offset of the DT_RUNPATH value word inside the fixture's dynamic
    /// segment, recovered via the decoder rather than hardcoded layout math.
    fn runpath_value_offset(elf: &[u8]) -> usize {
        let (little, phdrs) = some(decode_elf64(elf), "elf64");
        let dynamic = some(phdrs.iter().find(|p| p.p_type == PT_DYNAMIC), "dynamic");
        let dyn_off = dynamic.p_offset as usize;
        for i in 0..(dynamic.p_filesz as usize) / DYN64_LEN {
            let entry = dyn_off + i * DYN64_LEN;
            if some(rd_u64(elf, entry, little), "tag") == DT_RUNPATH {
                return entry + 8;
            }
        }
        panic!("fixture has no DT_RUNPATH");
    }

    #[test]
    fn elf_runpath_offset_outside_string_table_rejected_unchanged() {
        let mut elf = build_elf64(
            "@@HOMEBREW_PREFIX@@/lib/ld.so",
            "@@HOMEBREW_PREFIX@@/lib/foo",
        );
        let value_at = runpath_value_offset(&elf);
        // Point DT_RUNPATH past DT_STRSZ: the span leaves the mapped table.
        wr64(&mut elf, value_at, 1 << 20);
        let before = elf.clone();
        let result = relocate_bytes(&elf, "/opt/hb", false);
        assert!(matches!(result, Err(PourError::Relocation { .. })));
        assert_eq!(elf, before, "rejected input must stay byte-unchanged");
    }

    #[test]
    fn elf_string_table_past_end_of_file_rejected_unchanged() {
        let mut elf = build_elf64(
            "@@HOMEBREW_PREFIX@@/lib/ld.so",
            "@@HOMEBREW_PREFIX@@/lib/foo",
        );
        let (little, phdrs) = some(decode_elf64(&elf), "elf64");
        let dynamic = some(phdrs.iter().find(|p| p.p_type == PT_DYNAMIC), "dynamic");
        let dyn_off = dynamic.p_offset as usize;
        let total = elf.len() as u64;
        for i in 0..(dynamic.p_filesz as usize) / DYN64_LEN {
            let entry = dyn_off + i * DYN64_LEN;
            if some(rd_u64(&elf, entry, little), "tag") == DT_STRSZ {
                // Inflate DT_STRSZ so the table span leaves the file.
                wr64(&mut elf, entry + 8, total);
                break;
            }
        }
        let before = elf.clone();
        let result = relocate_bytes(&elf, "/opt/hb", false);
        assert!(matches!(result, Err(PourError::Relocation { .. })));
        assert_eq!(elf, before);
    }

    #[test]
    fn elf_grow_with_bss_in_final_load_rejected_unchanged() {
        let mut elf = build_elf64(
            "@@HOMEBREW_PREFIX@@/lib/ld.so",
            "@@HOMEBREW_PREFIX@@/lib/foo",
        );
        // Give the sole PT_LOAD a BSS tail: p_memsz = p_filesz + 64.
        let phoff = EHDR_MIN_LEN;
        let filesz = some(rd_u64(&elf, phoff + 32, true), "filesz");
        wr64(&mut elf, phoff + 40, filesz + 64);
        let before = elf.clone();
        let prefix = "/a/very/long/custom/homebrew/prefix/location/deep";
        let result = relocate_bytes(&elf, prefix, false);
        assert!(matches!(result, Err(PourError::Relocation { .. })));
        assert_eq!(elf, before);
    }

    #[test]
    fn elf_grow_with_nonterminal_final_load_rejected_unchanged() {
        let mut elf = build_elf64(
            "@@HOMEBREW_PREFIX@@/lib/ld.so",
            "@@HOMEBREW_PREFIX@@/lib/foo",
        );
        // Truncate the PT_LOAD file extent (memsz kept equal) so it no longer
        // ends the file.
        let phoff = EHDR_MIN_LEN;
        let filesz = some(rd_u64(&elf, phoff + 32, true), "filesz");
        wr64(&mut elf, phoff + 32, filesz - 8);
        wr64(&mut elf, phoff + 40, filesz - 8);
        let before = elf.clone();
        let prefix = "/a/very/long/custom/homebrew/prefix/location/deep";
        let result = relocate_bytes(&elf, prefix, false);
        assert!(matches!(result, Err(PourError::Relocation { .. })));
        assert_eq!(elf, before);
    }

    /// Hand-assemble a thin Mach-O 64 header with one `LC_RPATH` whose string
    /// slot has `pad` spare bytes after the NUL terminator.
    fn build_macho_rpath(rpath: &str, pad: usize) -> Vec<u8> {
        let str_off = MIN_LC_STR_OFFSET;
        let raw = str_off + rpath.len() + 1 + pad;
        let cmdsize = raw.div_ceil(8) * 8;
        let mut buf = vec![0u8; MACHO_HEADER64_LEN + cmdsize];
        wr32(&mut buf, 0, MH_MAGIC_64);
        wr32(&mut buf, 4, 0x0100_0007); // cputype x86_64
        wr32(&mut buf, 8, 0x3); // cpusubtype
        wr32(&mut buf, 12, 0x2); // filetype MH_EXECUTE
        wr32(&mut buf, 16, 1); // ncmds
        wr32(&mut buf, 20, cmdsize as u32); // sizeofcmds
        let base = MACHO_HEADER64_LEN;
        wr32(&mut buf, base, LC_RPATH);
        wr32(&mut buf, base + 4, cmdsize as u32);
        wr32(&mut buf, base + 8, str_off as u32);
        buf[base + str_off..base + str_off + rpath.len()].copy_from_slice(rpath.as_bytes());
        buf
    }

    #[test]
    fn macho_rpath_grows_into_trailing_nul_capacity() {
        // "/opt/homebrew-longer" (20 bytes) replaces the 19-byte placeholder:
        // longer than the original string, but inside the command slot.
        let macho = build_macho_rpath("@@HOMEBREW_PREFIX@@/lib", 8);
        let subs = subs_for("/opt/homebrew-longer");
        let mut buf = macho.clone();
        assert!(okr(macho_relocate(&mut buf, &macho, &subs), "relocate"));
        assert_eq!(buf.len(), macho.len(), "in-place rewrite only");
        let slots = some(okr(macho_string_slots(&buf), "slots"), "slots");
        assert_eq!(slots.len(), 1);
        assert_eq!(
            cstr(&buf[slots[0].start..slots[0].end]),
            b"/opt/homebrew-longer/lib"
        );
    }

    #[test]
    fn macho_rpath_exceeding_command_slot_rejected_unchanged() {
        let macho = build_macho_rpath("@@HOMEBREW_PREFIX@@/lib", 0);
        let subs = subs_for("/a/very/long/custom/homebrew/prefix/location/deep");
        let mut buf = macho.clone();
        let result = macho_relocate(&mut buf, &macho, &subs);
        assert!(result.is_err(), "overflow past cmdsize must be rejected");
        assert_eq!(buf, macho, "rejected image must stay byte-unchanged");
    }

    #[test]
    fn macho_rpath_rewrites_through_full_binary_pass() {
        let macho = build_macho_rpath("@@HOMEBREW_PREFIX@@/lib", 16);
        match relocate_bytes(&macho, "/opt/homebrew-longer", false) {
            Ok(Some((bytes, codesign))) => {
                assert!(codesign, "modified Mach-O must be re-signed");
                assert_eq!(bytes.len(), macho.len());
                let slots = some(okr(macho_string_slots(&bytes), "slots"), "slots");
                assert_eq!(
                    cstr(&bytes[slots[0].start..slots[0].end]),
                    b"/opt/homebrew-longer/lib"
                );
            }
            other => panic!("expected changed Mach-O, got {other:?}"),
        }
    }

    #[test]
    fn malformed_macho_command_table_rejected_unchanged() {
        let mut macho = build_macho_rpath("@@HOMEBREW_PREFIX@@/lib", 8);
        // Corrupt sizeofcmds so the command table leaves the file: a recognized
        // thin Mach-O with a malformed table must return a typed Relocation
        // error before the generic pass or codesign, leaving bytes unchanged.
        wr32(&mut macho, 20, u32::MAX);
        assert!(
            macho_string_slots(&macho).is_err(),
            "recognized malformed thin Mach-O must not decline silently"
        );
        let before = macho.clone();
        let result = relocate_bytes(&macho, "/opt/hb", false);
        match result {
            Err(PourError::Relocation { reason, .. }) => {
                assert!(reason.contains("malformed Mach-O"), "reason={reason}");
            }
            other => panic!("expected Relocation error, got {other:?}"),
        }
        assert_eq!(macho, before, "rejected image must stay byte-unchanged");
    }
}
