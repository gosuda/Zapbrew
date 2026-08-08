<div align="center">
  <h1>Zapbrew</h1>
  <p><strong>A Homebrew-compatible package manager, written in Rust.</strong></p>

  <a href="LICENSE"><img src="https://img.shields.io/badge/license-BSD--2--Clause-blue.svg" alt="License: BSD-2-Clause"></a>
  <img src="https://img.shields.io/badge/rust-1.97.1-orange.svg" alt="Rust 1.97.1">
  <img src="https://img.shields.io/badge/unsafe-forbidden-success.svg" alt="unsafe code forbidden">

  <br><br>

  <a href="docs/commands.md">Command reference</a> •
  <a href="docs/configuration.md">Configuration</a> •
  <a href="docs/how-it-works.md">How it works</a>
</div>

---

Zapbrew installs Homebrew bottles without Ruby. It reads the same signed JSON
catalogs that `brew` reads, writes the same prefix layout, and reads a
documented set of Homebrew's `HOMEBREW_*` variables. It is a single binary with
no runtime dependency on a Homebrew checkout.

```console
$ zapbrew install jq
==> Fetching oniguruma
==> Fetching jq
==> Pouring oniguruma--6.9.10.x86_64_linux.bottle.tar.gz
🍺  /tmp/zb-fresh/Cellar/oniguruma/6.9.10: 16 files, 1.7MB
==> Pouring jq--1.8.2.x86_64_linux.bottle.tar.gz
🍺  /tmp/zb-fresh/Cellar/jq/1.8.2: 21 files, 1.4MB
```

## Why Zapbrew

- **No Ruby.** The formula and cask catalogs are fetched as JSON from
  `formulae.brew.sh` and verified as JWS signatures. Zapbrew never evaluates a
  Ruby formula.
- **Compatible on disk.** Cellar, Caskroom, `opt`, `var/homebrew`, tabs, and
  install receipts follow Homebrew's layout, and `opt` symlinks point at the
  same keg paths. This is a byte-compatible layout, not a claim that a prefix
  can be safely shared with `brew` — see
  [Using it on an existing Homebrew prefix](#using-it-on-an-existing-homebrew-prefix).
- **Compatible in the environment.** Zapbrew reads a documented subset of
  Homebrew's `HOMEBREW_*` variables and defines none of its own. See
  [configuration](docs/configuration.md) for the supported list.
- **Safety enforced by the build.** The workspace sets
  `unsafe_code = "forbid"` and `clippy::unwrap_used = "deny"` for every crate.
- **Tested.** Per-crate unit tests, HTTP boundary tests against a local mock
  server, snapshot tests over user-visible output, and end-to-end command
  assertions.

## Requirements

- Rust 1.97.1. The version is pinned in `rust-toolchain.toml`, so `rustup`
  selects it automatically.
- Linux or macOS. Development and live verification for this repository run on
  x86_64 Linux. macOS support is compile-checked for `aarch64-apple-darwin` and
  covered by host-independent unit fixtures; it is not live-verified here.

## Quick start

Build the binary:

```bash
git clone https://github.com/gosuda/Zapbrew
cd Zapbrew
cargo build --release
```

Query the catalog. No prefix is written and nothing is installed:

```console
$ ./target/release/zapbrew info jq
==> jq: stable 1.8.2 (bottled)
Lightweight and flexible command-line JSON processor
https://jqlang.github.io/jq/
Not installed
From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/j/jq.rb
License: MIT
==> Dependencies
Required (1): oniguruma

$ ./target/release/zapbrew search ripgrep
==> Formulae
ripgrep
ripgrep-all
```

## Try it without touching your system

Point the prefix, Cellar, cache, logs, and staging directory at one scratch
location. This keeps the installation, downloads, logs, and temporary files
inside it:

```bash
export HOMEBREW_PREFIX=/tmp/zb-demo
export HOMEBREW_CELLAR=$HOMEBREW_PREFIX/Cellar
export HOMEBREW_CACHE=$HOMEBREW_PREFIX/cache
export HOMEBREW_LOGS=$HOMEBREW_PREFIX/logs
export HOMEBREW_TEMP=$HOMEBREW_PREFIX/tmp
mkdir -p "$HOMEBREW_CELLAR" "$HOMEBREW_CACHE" "$HOMEBREW_LOGS" "$HOMEBREW_TEMP"

./target/release/zapbrew install jq
"$HOMEBREW_PREFIX"/bin/jq --version   # jq-1.8.2
./target/release/zapbrew list --versions
```

Setting `HOMEBREW_PREFIX` alone is normally enough, because the Cellar defaults
to `$HOMEBREW_PREFIX/Cellar`. Set `HOMEBREW_CELLAR` as well when your shell has
sourced `brew shellenv`, which exports `HOMEBREW_CELLAR` and would otherwise
override the new prefix.

To remove the sandbox, delete `/tmp/zb-demo`.

## Using it on an existing Homebrew prefix

Zapbrew writes Homebrew's layout, so it can read an existing prefix and install
into it. That is a layout guarantee, not a concurrency guarantee: running both
managers against one prefix is outside what this documentation covers. When
relocation or linking fails mid-install, the transaction rolls back and reports
whatever it could not undo (see [how it works](docs/how-it-works.md)), but
reconciling a partial keg against `brew`'s own bookkeeping is left to you. Take
a backup before you first use it on a prefix you care about.

`zapbrew shim` manages an opt-in `brew` shim if you want existing scripts to
call Zapbrew. It is off by default.

Shell completions are generated at runtime. Each block writes to a
user-writable location and needs no root.

bash:

```bash
mkdir -p ~/.local/share/bash-completion/completions
zapbrew completions bash > ~/.local/share/bash-completion/completions/zapbrew
```

zsh — the directory must be on `fpath` before `compinit` runs:

```zsh
mkdir -p ~/.local/share/zsh/site-functions
zapbrew completions zsh > ~/.local/share/zsh/site-functions/_zapbrew
# then in ~/.zshrc, before compinit:
#   fpath+=("$HOME/.local/share/zsh/site-functions")
```

fish:

```fish
mkdir -p ~/.config/fish/completions
zapbrew completions fish > ~/.config/fish/completions/zapbrew.fish
```

## What Zapbrew does not do

Zapbrew is a package *consumer*. It has no formula-authoring surface.

- **No source builds.** `--build-from-source` is refused:
  `zapbrew cannot build from source: formulae are Ruby definitions. Use bottles (default) or brew.`
  A formula with no bottle for your platform cannot be installed.
- **No formula authoring.** `edit`, `create`, `audit`, `bottle`, `test`, and
  `bump` do not exist. Invoking them is an unrecognized-subcommand error.
- **No Ruby DSL, no tap formula evaluation.** Taps are cloned and inspected, but
  formulae are resolved from the JSON API.

Use `brew` for any of the above.

## Commands

30 commands. Full flags for each are in the
[command reference](docs/commands.md).

| Area | Commands |
|---|---|
| Install and remove | `install`, `reinstall`, `uninstall`, `upgrade`, `autoremove` |
| Inspect | `info`, `list`, `outdated`, `deps`, `uses`, `leaves`, `search`, `desc` |
| Link and pin | `link`, `unlink`, `pin`, `unpin` |
| Cache and cleanup | `fetch`, `cleanup` |
| Taps | `tap`, `untap`, `tap-info` |
| System | `config`, `doctor`, `shellenv`, `update`, `services`, `postinstall`, `shim`, `completions` |

Path queries short-circuit before any subcommand runs: `--prefix`, `--cellar`,
`--caskroom`, `--cache`, `--repository`, and `--taps`.

## Known differences from `brew`

- `zapbrew info` does not append `, HEAD` for a formula that has a HEAD spec.
- `zapbrew --version` reports `Homebrew 5-compatible`. It tracks Homebrew's CLI
  surface, not its version number.
- `install --cask --dry-run` is refused rather than performed. `brew` previews a
  cask; Zapbrew has no cask dry-run, so it refuses the combination before
  anything is downloaded instead of installing for real.
- `--cache <formula>` is refused. `brew --cache <formula>` prints that formula's
  download path; Zapbrew supports only bare `--cache`.
- `--HEAD` and `--interactive` are refused, like `--build-from-source`.

## Repository layout

| Crate | Responsibility |
|---|---|
| `zapbrew-types` | Names, versions, bottle tags, and other domain types |
| `zapbrew-prefix` | Environment detection and prefix layout |
| `zapbrew-net` | HTTP fetching, caching, and checksum verification |
| `zapbrew-api` | Signed JSON catalog loading and JWS verification |
| `zapbrew-pour` | Bottle unpacking, relocation, linking, and unlinking |
| `zapbrew-ops` | One module per command, with transactional install and rollback |
| `zapbrew-cli` | Argument parsing, dispatch, and output formatting |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

BSD-2-Clause. See [LICENSE](LICENSE).

Zapbrew is an independent project. It is not affiliated with or endorsed by
Homebrew.
