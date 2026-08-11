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
| `HOMEBREW_LOGS` | `~/Library/Logs/Homebrew` on macOS; `${XDG_CACHE_HOME:-~/.cache}/Homebrew/Logs` on Linux — parsed, not honored |
| `HOMEBREW_TEMP` | `/private/tmp` on macOS; `/var/tmp` on Linux when it is a readable and writable directory, otherwise `/tmp` — parsed, not honored |
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
| `HOMEBREW_API_DOMAIN` | `https://formulae.brew.sh/api` | Base URL for the signed formula and cask catalogs. Falls back to the default domain — see below |
| `HOMEBREW_API_AUTO_UPDATE_SECS` | `450` | Minimum seconds between automatic catalog refreshes |
| `HOMEBREW_NO_AUTO_UPDATE` | unset | Disables the automatic refresh |
| `HOMEBREW_DOWNLOAD_CONCURRENCY` | `auto` | `auto` means `available_parallelism * 2`. An integer is clamped to at least 1; an unparseable value becomes 1 |
| `HOMEBREW_BOTTLE_DOMAIN` | `https://ghcr.io/v2/homebrew/core` | Preferred bottle mirror. Exact HTTPS or `docker://` GHCR roots use OCI blob paths; other roots use bottle filenames. A failed custom mirror falls back to the catalog URL |
| `HOMEBREW_DOCKER_REGISTRY_TOKEN` | unset | Bearer token sent only to HTTPS GHCR artifact URLs |
| `HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN` | unset | Base64 basic-auth token used when the bearer token is unset. `none` suppresses the authorization header |
| `http_proxy`, `https_proxy`, `all_proxy`, `ftp_proxy`, `no_proxy` | unset | Read into `Env` (lowercase and uppercase spellings both accepted) and passed through to reqwest's system-proxy detection. `no_proxy` takes a comma-separated exclusion list; proxy URLs may carry credentials. Zapbrew's own config layer does not interpret these — `Env::proxy` is parsed and unused, and resolution is reqwest's standard environment handling |


### The custom API domain has a public fallback

When `HOMEBREW_API_DOMAIN` names a host other than the default and a catalog
request to it fails, Zapbrew retries that request once against
`https://formulae.brew.sh/api` (`zapbrew-api/src/transport.rs:296`). The retry is
unconditional: it sends no `If-Modified-Since`, so it is a full download.

This matches `brew`, whose own description of the variable says that if metadata
at that URL is temporarily unavailable, the default API domain is used as a
fallback mirror. Zapbrew uses the same guard `brew` does — the retry is skipped
when the configured domain already resolves to the default.

It has a consequence worth stating plainly: **an internal mirror does not confine
catalog traffic.** If you set this variable to keep catalog requests inside your
network, a mirror outage will send a request to the public service instead of
failing. Block the egress at the network layer if that matters; the variable is a
mirror preference, not a boundary.

## Install and cleanup behavior

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_NO_INSTALL_CLEANUP` | unset | Skip removal of replaced kegs after `upgrade` |
| `HOMEBREW_NO_AUTOREMOVE` | unset | Skip automatic autoremove after `uninstall` |
| `HOMEBREW_CLEANUP_MAX_AGE_DAYS` | `120` | Age threshold for stale cache entries |
| `HOMEBREW_NO_CLEANUP_FORMULAE` | empty | Protect matching installed kegs; cache scope is narrower |
| `HOMEBREW_NO_INSTALL_UPGRADE` | unset | When `install` names an already-linked outdated formula, leave it installed and print the explicit `upgrade` command. This does not change `upgrade` or cask installation |
| `HOMEBREW_FORBIDDEN_FORMULAE` | empty | Refuse requested formulae or dependencies whose short or full name appears in the list, before prefix mutation or download |
| `HOMEBREW_ALLOWED_TAPS` | empty | When non-empty, permit non-official formula taps and `tap` operations only when the tap name, owner (or `owner/*`), or evaluated remote matches an entry. Official taps at their default remote remain allowed. Formula installs evaluate the default remote; `tap` evaluates its supplied remote |
| `HOMEBREW_FORBIDDEN_TAPS` | empty | Refuse matching tap names, user names, or remote URLs |
| `HOMEBREW_FORBIDDEN_OWNER` | `you` | Name used in formula and tap policy refusal messages |

`HOMEBREW_NO_CLEANUP_FORMULAE` always excludes matching installed kegs from
`cleanup`. When you explicitly name a formula, it also excludes that formula's
cached bottles from `--scrub`. A bare cleanup does not use the list for cache
pruning. The list does not protect age-expired or incomplete downloads, prefix
cleanup, stale lock files, or formulae removed by `autoremove`.


## Accepted but not honored

The following variables are parsed into `Env` for Homebrew compatibility but no
production code path reads them. Setting them currently has no effect.

| Variable | Parsed as | Current consequence |
|---|---|---|
| `HOMEBREW_GITHUB_PACKAGES_TOKEN` | `Env::github_packages_token` | Parsed for compatibility. GitHub Packages authorization follows `HOMEBREW_DOCKER_REGISTRY_TOKEN`, then `HOMEBREW_DOCKER_REGISTRY_BASIC_AUTH_TOKEN`, then the anonymous `Bearer QQ==` token |
| `HOMEBREW_NO_EMOJI` | `Env::no_emoji` | The install badge (`HOMEBREW_INSTALL_BADGE`) is always printed after a successful pour; this variable does not suppress it |
| `HOMEBREW_NO_ENV_HINTS` | `Env::no_env_hints` | `link` still prints path hints for keg-only formulae |
| `HOMEBREW_FORBIDDEN_LICENSES` | `Env::forbidden_licenses` | No license check runs at install time; this list is not consulted |
| `HOMEBREW_DEBUG` | `Env::debug` | Merged from the variable and `--debug` in `main.rs:49`, then never read. No operation or reporter changes its output; there is no diagnostic mode |
| `HOMEBREW_LOGS` | `Env::logs` | Zapbrew writes no log files. The value is resolved during environment detection and never read again; the directory is not even created |
| `HOMEBREW_TEMP` | `Env::temp` | Not used for staging. Formula staging happens inside the Cellar rack, cask staging inside the Caskroom, and downloads inside the cache, so temporary I/O follows `HOMEBREW_CELLAR` and `HOMEBREW_CACHE` instead |

Do not treat the accepted-but-unhonored rows as enforcement or compliance
controls. Zapbrew accepts these variables, but they do not change its behavior.

## Output

| Variable | Default | Effect |
|---|---|---|
| `HOMEBREW_NO_COLOR`, `NO_COLOR` | unset | Either one disables color |
| `HOMEBREW_COLOR` | unset | Force color on, unless a no-color variable is set |
| `HOMEBREW_INSTALL_BADGE` | `🍺` | Badge printed after a successful pour |
| `HOMEBREW_VERBOSE` | unset | Merges into the verbose flag for most commands. Not fully equivalent to `--verbose`: `outdated` reads only the `--verbose` flag, so `HOMEBREW_VERBOSE` does not produce its version columns |

`HOMEBREW_NO_EMOJI`, `HOMEBREW_NO_ENV_HINTS`, and `HOMEBREW_DEBUG` (with its
`--debug` flag) are parsed but not honored — see
[Accepted but not honored](#accepted-but-not-honored).

List-valued variables split on commas and whitespace, and empty entries are
dropped. `HOMEBREW_API_AUTO_UPDATE_SECS` and `HOMEBREW_CLEANUP_MAX_AGE_DAYS` must
parse as unsigned integers; a malformed value is a startup error rather than a
silent fallback.
