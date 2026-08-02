# Grounded toolchain

Rust has no LTS channel. Crates.io crates do not publish LTS or support end dates. The chosen set uses the latest stable release unless the standard library removes the dependency.

## Pinned dependencies

| Item | Chosen | Release date | Source | Decision and current pattern |
|---|---:|---:|---|---|
| Rust | 1.97.1 | 2026-07-16 | https://static.rust-lang.org/dist/channel-rust-stable.toml | Edition 2024, resolver 3, `rust-version = "1.97"`. |
| clap | 4.6.5 | 2026-07-31 | https://crates.io/crates/clap/4.6.5 | Derive `Parser`, `Subcommand`, and `Args`. |
| clap_complete | 4.6.8 | 2026-07-27 | https://crates.io/crates/clap_complete/4.6.8 | Generate from `CommandFactory::command()`. |
| tokio | 1.53.1 | 2026-07-20 | https://crates.io/crates/tokio/1.53.1 | Multi-thread runtime with fs, process, signal, and macros. |
| reqwest | 0.13.4 | 2026-05-25 | https://crates.io/crates/reqwest/0.13.4 | Disable defaults. Enable rustls, gzip, brotli, zstd, stream, and json. |
| serde | 1.0.229 | 2026-07-18 | https://crates.io/crates/serde/1.0.229 | Derive typed models. |
| serde_json | 1.0.151 | 2026-07-20 | https://crates.io/crates/serde_json/1.0.151 | Merge variations as `Value`, then deserialize. |
| rsa | 0.9.10 | 2026-01-15 | https://crates.io/crates/rsa/0.9.10 | Verify PS512 with `Pss::new_with_salt::<Sha512>(64)`. |
| sha2 | 0.11.0 | 2026-03-10 | https://crates.io/crates/sha2/0.11.0 | Stream hashes through `Digest`. |
| base64 | 0.23.0 | 2026-07-23 | https://crates.io/crates/base64/0.23.0 | Added because JWS needs URL-safe no-pad decoding. |
| bzip2 | 0.6.1 | 2025-10-16 | https://crates.io/crates/bzip2/0.6.1 | Cask `.tar.*` archive decoding only; default backend `libbz2-rs-sys` avoids new external binaries or C libraries. |
| tar | 0.4.46 | 2025-11-12 | https://crates.io/crates/tar/0.4.46 | Validate entry prefixes, then use `Entry::unpack_in`. |
| flate2 | 1.1.9 | 2026-02-03 | https://crates.io/crates/flate2/1.1.9 | Stream through `read::GzDecoder`. |
| object | 0.40.0 | 2026-08-01 | https://crates.io/crates/object/0.40.0 | Enable read and write for ELF and Mach-O. This direct registry check overrides one stale research report. |
| camino | 1.2.5 | 2026-02-14 | https://crates.io/crates/camino/1.2.5 | Keep all modeled paths UTF-8. |
| thiserror | 2.0.19 | 2026-07-18 | https://crates.io/crates/thiserror/2.0.19 | Typed library error enums. |
| jiff | 0.2.35 | 2026-07-25 | https://crates.io/crates/jiff/0.2.35 | Timestamps and HTTP-date formatting. |
| lzma-rust2 | 0.18.0 | 2026-07-26 | https://crates.io/crates/lzma-rust2/0.18.0 | Cask `.tar.*` archive decoding only; pure-Rust LZMA avoids new external binaries or C libraries. |
| indicatif | 0.18.6 | 2026-07-01 | https://crates.io/crates/indicatif/0.18.6 | TTY-only download progress. |
| rustix | 1.1.4 | 2026-02-22 | https://crates.io/crates/rustix/1.1.4 | Unix metadata, permissions, and symlinks without direct libc. |
| ruzstd | 0.9.0 | 2026-07-26 | https://crates.io/crates/ruzstd/0.9.0 | Cask `.tar.*` archive decoding only; pure-Rust Zstandard avoids new external binaries or C libraries. |
| regex | 1.13.1 | 2026-07-15 | https://crates.io/crates/regex/1.13.1 | Version and search parsing. |
| owo-colors | 4.3.0 | 2026-02-22 | https://crates.io/crates/owo-colors/4.3.0 | Stream-aware conditional ANSI output. |
| plist | 1.10.0 | 2026-07-04 | https://crates.io/crates/plist/1.10.0 | Serialize launchd XML with Serde. |
| jaro_winkler | 0.2.1 | 2026-03-31 | https://crates.io/crates/jaro_winkler/0.2.1 | Replaces stale `strsim`; compare its `f32` score with `0.8`. |
| zip | 8.6.0 | 2026-04-25 | https://crates.io/crates/zip/8.6.0 | Require `ZipFile::enclosed_name()` before extraction. |
| futures | 0.3.33 | 2026-07-18 | https://crates.io/crates/futures/0.3.33 | Bound downloads with `buffer_unordered`. |
| terminal_size | 0.4.4 | 2026-03-23 | https://crates.io/crates/terminal_size/0.4.4 | Query terminal dimensions for column output. |
| anyhow | 1.0.104 | 2026-07-18 | https://crates.io/crates/anyhow/1.0.104 | CLI main only. Libraries keep typed errors. |
| insta | 1.48.0 | 2026-06-11 | https://crates.io/crates/insta/1.48.0 | Snapshot only user-visible behavior. |
| assert_cmd | 2.2.2 | 2026-05-11 | https://crates.io/crates/assert_cmd/2.2.2 | End-to-end command assertions. |
| wiremock | 0.6.5 | 2025-08-24 | https://crates.io/crates/wiremock/0.6.5 | Deterministic HTTP boundary tests. |
| tempfile | 3.27.0 | 2026-03-11 | https://crates.io/crates/tempfile/3.27.0 | Isolated prefixes and caches. |

## Removed pins

- `fd-lock 4.0.4` had no release in 12 months. Rust 1.97 `std::fs::File::try_lock` has the required nonblocking `flock` behavior and has been stable since Rust 1.89: https://doc.rust-lang.org/1.97.1/std/fs/struct.File.html#method.try_lock
- `walkdir 2.5.0` had no release in 12 months. Recursive `std::fs::read_dir` covers the bounded traversal contract without a dependency.
- `strsim 0.11.1` had no release in 12 months. `jaro_winkler 0.2.1` is current and supplies the single metric used here: https://docs.rs/jaro_winkler/0.2.1/jaro_winkler/
- Direct `libc` and `num_cpus` pins are not needed. Use `rustix` and `std::thread::available_parallelism`.

## Platform baseline

- Linux and macOS follow Rust 1.97 supported targets. The project does not pin an OS release floor.
- Live verification runs on Linux. macOS is compile-checked for `aarch64-apple-darwin` and covered by host-independent unit fixtures.
