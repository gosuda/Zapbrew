# How Zapbrew works

Homebrew's formulae are Ruby files. Zapbrew never evaluates them. Everything it
knows about a package comes from the signed JSON catalogs that Homebrew also
publishes, and everything it installs is a prebuilt bottle. That single decision
shapes the whole design, including what Zapbrew
[cannot do](../README.md#what-zapbrew-does-not-do).

## The catalog

`zapbrew-api` loads `formula.jws.json` and `cask.jws.json` from
`HOMEBREW_API_DOMAIN`, default `https://formulae.brew.sh/api`.

These files are JWS envelopes. The payload is verified against Homebrew's
built-in RSA public key using PSS with SHA-512 before anything is deserialized. A
payload that does not verify is rejected rather than used.

Fetching is conditional. When a usable cache entry exists, the request carries
`If-Modified-Since`, and a `304 Not Modified` response reuses the cached bytes.
`HOMEBREW_API_AUTO_UPDATE_SECS` bounds how often this happens;
`HOMEBREW_NO_AUTO_UPDATE` disables it.

A command loads only the catalogs it reads. `dispatch.rs` classifies each command
with `needs_formula` and `needs_cask`, and a command that reads neither gets an
empty catalog instead of a download. Path queries such as `--prefix` answer
before any of this runs.

## Choosing a bottle

The host produces a bottle tag: operating system, architecture, and on macOS the
product version from `sw_vers -productVersion`. A formula with no bottle for that
tag cannot be installed, because building from source is out of scope.

Dependency resolution walks the catalog graph. Build, test, recommended, and
optional edges are filtered according to the requested flags, so a runtime
install does not pull build-only dependencies.

## Installing

`install::run` resolves the request in stages. A normal install prepares prefix
paths and creates tap and formula lock files before it checks conflicts:

1. Refuse Ruby-only modes such as `--build-from-source`.
2. Resolve each requested name to a catalog formula, following aliases.
3. Expand dependencies and deduplicate the candidate list.
4. Refuse disabled formulae and report deprecated ones.
5. Collect affected names, including anything in `conflicts_with`.
6. For a normal install, run prefix setup, take per-tap and per-formula locks,
   and scan what is installed. Prefix setup includes Linux runtime symlinks
   where applicable, and the lock files remain after the locks are released.
7. Drop candidates that are already satisfied unless `--force`.
8. Check conflicts.

`--dry-run` takes a non-mutating path at step 6: it scans installed state
without prefix setup or locks. It then prints what would be installed and makes
no changes to the Cellar or prefix. It does not skip the catalog, so a refresh
may still write to the API cache.

Downloads then run concurrently, bounded by
`HOMEBREW_DOWNLOAD_CONCURRENCY`, and every artifact is checked against the
catalog's checksum before use.

## The pour, and how it unwinds

Each formula is poured inside a transaction. The transaction journals the
keg-install mutations it makes under the Cellar and prefix, so a failure before
commit can be reversed. Downloads and cache writes happen earlier and are not
part of it. In order:

| Step | Journal entry that makes it reversible |
|---|---|
| Create the rack directory | `created_dirs` |
| Create a staging directory | `stage_root` |
| Unpack the bottle into staging | staging is removable |
| Relocate paths in text, ELF, and Mach-O files | staging is removable |
| Write `INSTALL_RECEIPT.json` | inside staging |
| Unlink the previously linked keg, if replacing | `old_unlinked` |
| Rename the old keg aside, if replacing | `backup` |
| Rename staging into its final keg path | `promoted` |
| Link the keg into the prefix | `new_link_attempted` |
| Copy the prefix skeleton | `skeleton` |
| Run post-install steps | filesystem steps record a `steps` inverse; `run` and maintenance steps are executed as processes and record no inverse |

Only after all of that does the journal set `committed`. Before that point, a
failure triggers `rollback`, which walks the journal in reverse: undo the
journaled filesystem post-install steps, remove copied skeleton entries, unlink
the new keg, remove the promoted keg, restore the backup, relink the old keg,
remove staging, and remove created directories.

Rollback is honest about failure. Anything it could not undo is collected as
leftovers and reported in the error, rather than being silently ignored.

**Executed post-install steps are outside this guarantee.** A `run` or
maintenance step is handed to `run_command` as a process, and no inverse is
recorded for it. If a later step fails, rollback cannot undo what that command
did to the system, and it does not appear in the leftovers either — the journal
has no entry for it. Treat a formula whose post-install plan executes commands as
not fully reversible.

After commit, the backup and step scratch directories are removed. If that
cleanup fails, the paths are reported too.

The transaction's destructive steps are confined: paths are checked to fall
inside the Cellar or prefix, and a target that is a symlink where a real
directory was expected is an error rather than something to follow.

## Errors

Libraries return typed `thiserror` errors. `anyhow` appears only in the CLI entry
point, so no library flattens an error into a string before the CLI decides how
to render it.

For what each crate owns, see
[Repository layout](../README.md#repository-layout).
