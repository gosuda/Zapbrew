# Configuration

Zapbrew reads Homebrew's environment variables for compatibility. It defines no
`ZAPBREW_*` variables, so an existing Homebrew environment needs no changes. Not
every variable Zapbrew parses is acted on — see
[Accepted but not honored](#accepted-but-not-honored) for the ones that currently
have no effect.

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
often enough to relocate an installation, but prefix-only relocation is not
guaranteed: a link operation that fails partway leaves the prefix in a mixed
state. Zapbrew rolls back the filesystem steps it can undo and reports any
leftovers it could not — see [how it works](how-it-works.md) for the transaction
model. One caveat that applies regardless: `brew shellenv` exports
`HOMEBREW_CELLAR` explicitly, so in a shell that has sourced it, an inherited
`HOMEBREW_CELLAR` overrides the new prefix. Unset it, or set it alongside the
prefix — see the sandbox recipe in the
[README](../README.md#try-it-without-touching-your-system).

## Network

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_API_DOMAIN` | `https://formulae.brew.sh/api` | Base URL for the signed formula and cask catalogs |
| `HOMEBREW_API_AUTO_UPDATE_SECS` | `450` | Minimum seconds between automatic catalog refreshes |
| `HOMEBREW_NO_AUTO_UPDATE` | unset | Disables the automatic refresh |
| `HOMEBREW_DOWNLOAD_CONCURRENCY` | `auto` | `auto` means `available_parallelism * 2`. An integer is clamped to at least 1; an unparseable value becomes 1 |
| `HOMEBREW_GITHUB_PACKAGES_TOKEN` | unset | Bearer token for the bottle registry |
| `HOMEBREW_DOCKER_REGISTRY_TOKEN` | unset | Bearer token for a Docker-style registry |
| `http_proxy`, `https_proxy`, `all_proxy`, `ftp_proxy`, `no_proxy` | unset | Read into `Env` (lowercase and uppercase spellings both accepted) and passed through to reqwest's system-proxy detection. `no_proxy` takes a comma-separated exclusion list; proxy URLs may carry credentials. Zapbrew's own config layer does not interpret these — `Env::proxy` is parsed and unused, and resolution is reqwest's standard environment handling |

`HOMEBREW_BOTTLE_DOMAIN` is parsed but not honored — see
[Accepted but not honored](#accepted-but-not-honored).

## Install and cleanup behavior

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_NO_INSTALL_CLEANUP` | unset | Skip the cleanup that follows an install |
| `HOMEBREW_NO_AUTOREMOVE` | unset | Do not autoremove unused dependencies |
| `HOMEBREW_CLEANUP_MAX_AGE_DAYS` | `120` | Age threshold for stale cache entries |
| `HOMEBREW_NO_CLEANUP_FORMULAE` | empty | Formulae `cleanup` must never touch |

`HOMEBREW_NO_INSTALL_UPGRADE` is parsed but not honored — see
[Accepted but not honored](#accepted-but-not-honored).

## Accepted but not honored

The following variables are parsed into `Env` for Homebrew compatibility but no
production code path reads them. Setting them currently has no effect.

| Variable | Parsed as | Current consequence |
|---|---|---|
| `HOMEBREW_BOTTLE_DOMAIN` | `Env::bottle_domain` | Downloads use the catalog's `BottleFile::url` directly, so an internal mirror is ignored; installs contact the URL the catalog names (typically `ghcr.io`) regardless of this setting |
| `HOMEBREW_NO_INSTALL_UPGRADE` | `Env::no_install_upgrade` | `install` skips only an already-current linked version; an installed formula that is behind its catalog version is still upgraded |
| `HOMEBREW_NO_EMOJI` | `Env::no_emoji` | The install badge (`HOMEBREW_INSTALL_BADGE`) is always printed after a successful pour; this variable does not suppress it |
| `HOMEBREW_NO_ENV_HINTS` | `Env::no_env_hints` | `link` still prints path hints for keg-only formulae |
| `HOMEBREW_FORBIDDEN_FORMULAE` | `Env::forbidden_formulae` | `install` checks only catalog `disabled`/`deprecated` flags; this list is not consulted |
| `HOMEBREW_FORBIDDEN_TAPS` | `Env::forbidden_taps` | `tap` never consults tap policy; this list is not enforced |
| `HOMEBREW_FORBIDDEN_LICENSES` | `Env::forbidden_licenses` | No license check runs at install time; this list is not consulted |
| `HOMEBREW_ALLOWED_TAPS` | `Env::allowed_taps` | `tap` never consults tap policy; this allow-list is not enforced |
| `HOMEBREW_FORBIDDEN_OWNER` | `Env::forbidden_owner` | No refusal message references this value; it is unused |

These are **not** an enforcement or compliance control. Do not rely on them to
restrict what Zapbrew will install or tap.

## Output

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_NO_COLOR`, `NO_COLOR` | unset | Either one disables color |
| `HOMEBREW_COLOR` | unset | Force color on, unless a no-color variable is set |
| `HOMEBREW_INSTALL_BADGE` | `🍺` | Badge printed after a successful pour |
| `HOMEBREW_DEBUG` | unset | Same as `--debug` |
| `HOMEBREW_VERBOSE` | unset | Merges into the verbose flag for most commands. Not fully equivalent to `--verbose`: `outdated` reads only the `--verbose` flag, so `HOMEBREW_VERBOSE` does not produce its version columns |

`HOMEBREW_NO_EMOJI` and `HOMEBREW_NO_ENV_HINTS` are parsed but not honored — see
[Accepted but not honored](#accepted-but-not-honored).

List-valued variables split on commas and whitespace, and empty entries are
dropped. `HOMEBREW_API_AUTO_UPDATE_SECS` and `HOMEBREW_CLEANUP_MAX_AGE_DAYS` must
parse as unsigned integers; a malformed value is a startup error rather than a
silent fallback.
