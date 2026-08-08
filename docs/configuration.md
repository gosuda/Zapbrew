# Configuration

Zapbrew honors Homebrew's environment variables for compatibility. It defines no
`ZAPBREW_*` variables, so an existing Homebrew environment needs no changes.

The variables listed here are the ones Zapbrew acts on. They come from
`TRACKED_VARS` in
[`crates/zapbrew-prefix/src/env.rs`](../crates/zapbrew-prefix/src/env.rs), which
is also the set captured when environment-dependent state is compared. Zapbrew
additionally reads `HOME` and `XDG_CACHE_HOME` to resolve default paths, and on
macOS it runs `sw_vers -productVersion` to determine the bottle tag.

Run `zapbrew config` to print the values in effect. Like `brew config`, it omits
path variables that match their default.

## Paths

| Variable | Default |
|---|---|
| `HOMEBREW_PREFIX` | `/home/linuxbrew/.linuxbrew` on Linux, `/opt/homebrew` on arm64 macOS, `/usr/local` on x86_64 macOS |
| `HOMEBREW_CELLAR` | `$HOMEBREW_PREFIX/Cellar` |
| `HOMEBREW_CACHE` | `~/Library/Caches/Homebrew` on macOS; `${XDG_CACHE_HOME:-~/.cache}/Homebrew` on Linux |
| `HOMEBREW_LOGS` | `~/Library/Logs/Homebrew` on macOS; `${XDG_CACHE_HOME:-~/.cache}/Homebrew/Logs` on Linux |
| `HOMEBREW_TEMP` | `/private/tmp` on macOS; `/var/tmp` on Linux when it is a readable and writable directory, otherwise `/tmp` |
| `HOMEBREW_REPOSITORY` | `$HOMEBREW_PREFIX` on arm64 macOS, `$HOMEBREW_PREFIX/Homebrew` elsewhere |

`Caskroom`, `Library`, `var/homebrew/locks`, `var/homebrew/pinned`, and
`var/homebrew/linked` are derived from the prefix and repository; they have no
variables of their own.

Because the Cellar defaults to `$HOMEBREW_PREFIX/Cellar`, setting the prefix is
usually enough to relocate an installation. One caveat: `brew shellenv` exports
`HOMEBREW_CELLAR` explicitly, so in a shell that has sourced it, an inherited
`HOMEBREW_CELLAR` overrides the new prefix. Unset it, or set it alongside the
prefix — see the sandbox recipe in the
[README](../README.md#try-it-without-touching-your-system).

## Network

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_API_DOMAIN` | `https://formulae.brew.sh/api` | Base URL for the signed formula and cask catalogs |
| `HOMEBREW_BOTTLE_DOMAIN` | `https://ghcr.io/v2/homebrew/core` | Bottle registry |
| `HOMEBREW_API_AUTO_UPDATE_SECS` | `450` | Minimum seconds between automatic catalog refreshes |
| `HOMEBREW_NO_AUTO_UPDATE` | unset | Disables the automatic refresh |
| `HOMEBREW_DOWNLOAD_CONCURRENCY` | `auto` | `auto` means `available_parallelism * 2`. An integer is clamped to at least 1; an unparseable value becomes 1 |
| `HOMEBREW_GITHUB_PACKAGES_TOKEN` | unset | Bearer token for the bottle registry |
| `HOMEBREW_DOCKER_REGISTRY_TOKEN` | unset | Bearer token for a Docker-style registry |
| `http_proxy`, `https_proxy`, `all_proxy`, `ftp_proxy`, `no_proxy` | unset | Standard proxy variables; the uppercase spellings are read as well |

## Install and cleanup behavior

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_NO_INSTALL_CLEANUP` | unset | Skip the cleanup that follows an install |
| `HOMEBREW_NO_INSTALL_UPGRADE` | unset | `install` on an already-installed formula does not upgrade it |
| `HOMEBREW_NO_AUTOREMOVE` | unset | Do not autoremove unused dependencies |
| `HOMEBREW_CLEANUP_MAX_AGE_DAYS` | `120` | Age threshold for stale cache entries |
| `HOMEBREW_NO_CLEANUP_FORMULAE` | empty | Formulae `cleanup` must never touch |

## Policy

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_FORBIDDEN_FORMULAE` | empty | Formulae refused at install time |
| `HOMEBREW_FORBIDDEN_TAPS` | empty | Taps refused |
| `HOMEBREW_FORBIDDEN_LICENSES` | empty | Licenses refused |
| `HOMEBREW_ALLOWED_TAPS` | empty | When set, only these taps are permitted |
| `HOMEBREW_FORBIDDEN_OWNER` | `you` | Name used in the refusal message |

List-valued variables split on commas and whitespace, and empty entries are
dropped.

## Output

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_NO_COLOR`, `NO_COLOR` | unset | Either one disables color |
| `HOMEBREW_COLOR` | unset | Force color on, unless a no-color variable is set |
| `HOMEBREW_NO_EMOJI` | unset | Suppress the install badge |
| `HOMEBREW_INSTALL_BADGE` | `🍺` | Badge printed after a successful pour |
| `HOMEBREW_NO_ENV_HINTS` | unset | Suppress environment hints |
| `HOMEBREW_DEBUG` | unset | Same as `--debug` |
| `HOMEBREW_VERBOSE` | unset | Same as `--verbose` |

`HOMEBREW_API_AUTO_UPDATE_SECS` and `HOMEBREW_CLEANUP_MAX_AGE_DAYS` must parse
as unsigned integers; a malformed value is a startup error rather than a
silent fallback.
