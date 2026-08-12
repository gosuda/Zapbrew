# Command reference

Every fenced block below is the verbatim output of `zapbrew <command> --help` from `zapbrew 0.1.0 (Homebrew 5-compatible)`.
If a flag is not listed here, this build does not accept it.

Some accepted flags are refused at runtime. Those are called out in a quoted note
directly beneath the help block they belong to. See also the
[accepted but not honored](configuration.md#accepted-but-not-honored) environment
variables.

30 commands. See [configuration.md](configuration.md) for environment variables.

## Global options and path queries

A path query short-circuits before any subcommand runs and prints one line.
At most one may be given.

```text
Options:
      --debug               Enable debug output
  -q, --quiet               Suppress non-essential output
  -v, --verbose             Enable verbose output
      --prefix [<formula>]  Print the install prefix, optionally for a formula. Resolved later
      --cellar [<formula>]  Print the Cellar path, optionally for a formula. Resolved later
      --caskroom [<cask>]   Print the Caskroom path, optionally for a cask token. Resolved later
      --cache [<formula>]   Print the download cache path, optionally for a formula. Resolved later
      --repository [<tap>]  Print the repository path, optionally for a tap. Resolved later
      --taps                Print the Taps directory path
  -h, --help                Print help
  -V, --version             Print version
```

> **`--cache` does not accept a formula name.** The help text shows
> `--cache [<formula>]`, but passing a name is refused with
> `--cache with a formula name is not supported.` Bare `--cache` prints the
> download cache path and works as documented.
>
> **`--debug` changes nothing.** The flag and `HOMEBREW_DEBUG` are merged into
> `Env::debug`, which no operation and no reporter reads. This build has no
> diagnostic mode; `--verbose` is the flag that affects output. See
> [accepted but not honored](configuration.md#accepted-but-not-honored).

## Commands

| Command | Purpose |
|---|---|
| [`install`](#zapbrew-install) | Install formulae or casks |
| [`reinstall`](#zapbrew-reinstall) | Reinstall formulae |
| [`uninstall`](#zapbrew-uninstall) | Uninstall formulae or casks |
| [`upgrade`](#zapbrew-upgrade) | Upgrade outdated formulae |
| [`outdated`](#zapbrew-outdated) | List outdated formulae and casks |
| [`list`](#zapbrew-list) | List installed formulae or casks |
| [`info`](#zapbrew-info) | Show information about formulae and casks |
| [`deps`](#zapbrew-deps) | Show dependencies of formulae |
| [`uses`](#zapbrew-uses) | Show formulae and casks that depend on the named formulae or casks |
| [`leaves`](#zapbrew-leaves) | List installed formulae not required by others |
| [`autoremove`](#zapbrew-autoremove) | Uninstall formulae that are no longer needed |
| [`pin`](#zapbrew-pin) | Pin formulae, preventing upgrades |
| [`unpin`](#zapbrew-unpin) | Unpin formulae, allowing upgrades |
| [`link`](#zapbrew-link) | Symlink a keg's files into the prefix |
| [`unlink`](#zapbrew-unlink) | Remove a keg's symlinks from the prefix |
| [`fetch`](#zapbrew-fetch) | Download bottles without installing |
| [`cleanup`](#zapbrew-cleanup) | Remove stale downloads and old versions |
| [`search`](#zapbrew-search) | Search for formulae and casks |
| [`desc`](#zapbrew-desc) | Show descriptions or search names/descriptions of formulae/casks |
| [`postinstall`](#zapbrew-postinstall) | Rerun post-install steps for installed formulae |
| [`config`](#zapbrew-config) | Show the effective configuration |
| [`shellenv`](#zapbrew-shellenv) | Print shell integration for the environment |
| [`tap`](#zapbrew-tap) | Tap a formula repository, or list taps |
| [`untap`](#zapbrew-untap) | Remove a tapped repository |
| [`tap-info`](#zapbrew-tap-info) | Show information about a tap |
| [`doctor`](#zapbrew-doctor) | Check the system for potential problems |
| [`services`](#zapbrew-services) | Manage background services |
| [`update`](#zapbrew-update) | Fetch the latest catalog and taps |
| [`shim`](#zapbrew-shim) | Manage the opt-in `brew` shim |
| [`completions`](#zapbrew-completions) | Manage shell completion links |

### `zapbrew install`

Install formulae or casks.

```text
Install formulae or casks

Usage: zapbrew install [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula or cask names to install

Options:
      --debug              Enable debug output
      --only-dependencies  Install only the dependencies, not the formulae themselves
  -f, --force              Install even if already installed
  -q, --quiet              Suppress non-essential output
  -n, --dry-run            Show what would be installed without doing it
  -v, --verbose            Enable verbose output
      --build-from-source  Compile from source instead of pouring a bottle
      --HEAD               Install the HEAD version
  -i, --interactive        Run the installation interactively
      --include-test       Include test dependencies during expansion
      --cask               Treat the named arguments as casks (macOS only)
      --formula            Treat the named arguments as formulae
      --appdir <DIR>       Target application directory for cask apps
  -h, --help               Print help
```

> **Modes this build refuses.** Each returns an error and installs nothing.
>
> In formula mode, three flags are parser-compatible but rejected at runtime:
>
> - `--build-from-source` — `zapbrew cannot build from source: formulae are Ruby definitions. Use bottles (default) or brew.`
> - `--HEAD` — `zapbrew cannot install HEAD formulae: formulae are Ruby definitions. Use bottled stable releases or brew.`
> - `--interactive` — `zapbrew cannot install interactively: formulae are Ruby definitions. Use bottles (default) or brew.`
>
> In cask mode, only `--force` and `--appdir` are carried through. Every other
> install flag is refused rather than silently dropped — `--only-dependencies`,
> `--dry-run`, `--build-from-source`, `--HEAD`, `--interactive` and
> `--include-test` — with a message naming the flag, for example
> `zapbrew cannot honor --dry-run with --cask: the cask install path does not support it. Use brew.`
>
> The refusal matters most for `--dry-run` and `--only-dependencies`: accepting
> either and installing anyway would mutate the Caskroom and the application
> directory that the flag asked it to leave alone. Both work as documented in
> formula mode.

### `zapbrew reinstall`

Reinstall formulae.

```text
Reinstall formulae

Usage: zapbrew reinstall [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew uninstall`

Uninstall formulae or casks.

```text
Uninstall formulae or casks

Usage: zapbrew uninstall [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula or cask names to uninstall

Options:
      --debug                Enable debug output
  -f, --force                Delete all installed versions, not just the active one
      --ignore-dependencies  Do not check for dependents before uninstalling
  -q, --quiet                Suppress non-essential output
      --cask                 Treat the named arguments as casks (macOS only)
  -v, --verbose              Enable verbose output
      --formula              Treat the named arguments as formulae
      --zap                  Also remove all files a cask created (cask only)
  -h, --help                 Print help
```

### `zapbrew upgrade`

Upgrade outdated formulae.

```text
Upgrade outdated formulae

Usage: zapbrew upgrade [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names to upgrade; empty upgrades all

Options:
      --debug    Enable debug output
  -n, --dry-run  Show what would be upgraded without doing it
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew outdated`

List outdated formulae and casks.

```text
List outdated formulae and casks

Usage: zapbrew outdated [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula or cask names to check; empty checks all installed formulae and casks

Options:
      --debug                Enable debug output
      --json[=<v2>]          Emit JSON output (v2 schema) [possible values: v2]
      --greedy               Include casks with auto-updates or `latest` versions
  -q, --quiet                Suppress non-essential output
      --greedy-latest        Include casks whose version is `latest`
  -v, --verbose              Enable verbose output
      --greedy-auto-updates  Include casks that update themselves
  -h, --help                 Print help
```

### `zapbrew list`

List installed formulae or casks.

```text
List installed formulae or casks

Usage: zapbrew list [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula or cask names to list; empty lists all formulae unless --cask is set

Options:
      --debug     Enable debug output
      --versions  Show version numbers
  -1              Print one entry per line
  -q, --quiet     Suppress non-essential output
      --cask      List casks instead of formulae (macOS only)
  -v, --verbose   Enable verbose output
      --formula   Treat the named arguments as formulae
  -h, --help      Print help
```

### `zapbrew info`

Show information about formulae and casks.

```text
Show information about formulae and casks

Usage: zapbrew info [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula or cask names to describe

Options:
      --debug        Enable debug output
      --json[=<v2>]  Emit JSON output (v2 schema) [possible values: v2]
  -q, --quiet        Suppress non-essential output
  -v, --verbose      Enable verbose output
  -h, --help         Print help
```

### `zapbrew deps`

Show dependencies of formulae.

```text
Show dependencies of formulae

Usage: zapbrew deps [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names

Options:
      --debug             Enable debug output
      --tree              Render the dependency graph as a tree
  -q, --quiet             Suppress non-essential output
      --union             Show the union of dependencies across all arguments
      --include-build     Include `:build` dependencies
  -v, --verbose           Enable verbose output
      --include-test      Include `:test` dependencies
      --include-optional  Include `:optional` dependencies
      --skip-recommended  Skip `:recommended` dependencies
  -h, --help              Print help
```

### `zapbrew uses`

Show formulae and casks that depend on the named formulae or casks.

```text
Show formulae and casks that depend on the named formulae or casks

Usage: zapbrew uses [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula or cask names

Options:
      --debug             Enable debug output
  -r, --recursive         Resolve the reverse-dependency closure recursively
      --installed         Restrict results to installed formulae and casks
  -q, --quiet             Suppress non-essential output
      --include-build     Include `:build` dependencies
  -v, --verbose           Enable verbose output
      --include-test      Include `:test` dependencies
      --include-optional  Include `:optional` dependencies
      --skip-recommended  Skip `:recommended` dependencies
  -h, --help              Print help
```

### `zapbrew leaves`

List installed formulae not required by others.

```text
List installed formulae not required by others

Usage: zapbrew leaves [OPTIONS]

Options:
      --debug                    Enable debug output
  -r, --installed-on-request     Only list formulae installed on request
  -p, --installed-as-dependency  Only list formulae installed as a dependency
  -q, --quiet                    Suppress non-essential output
  -v, --verbose                  Enable verbose output
  -h, --help                     Print help
```

### `zapbrew autoremove`

Uninstall formulae that are no longer needed.

```text
Uninstall formulae that are no longer needed

Usage: zapbrew autoremove [OPTIONS]

Options:
      --debug    Enable debug output
  -n, --dry-run  Show what would be removed without doing it
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew pin`

Pin formulae, preventing upgrades.

```text
Pin formulae, preventing upgrades

Usage: zapbrew pin [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew unpin`

Unpin formulae, allowing upgrades.

```text
Unpin formulae, allowing upgrades

Usage: zapbrew unpin [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew link`

Symlink a keg's files into the prefix.

```text
Symlink a keg's files into the prefix

Usage: zapbrew link [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names to link

Options:
      --debug      Enable debug output
      --overwrite  Delete files that already exist in the prefix
  -n, --dry-run    Show what would be linked without doing it
  -q, --quiet      Suppress non-essential output
  -f, --force      Force linking even for keg-only formulae
  -v, --verbose    Enable verbose output
  -h, --help       Print help
```

### `zapbrew unlink`

Remove a keg's symlinks from the prefix.

```text
Remove a keg's symlinks from the prefix

Usage: zapbrew unlink [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names to unlink

Options:
      --debug    Enable debug output
  -n, --dry-run  Show what would be removed without doing it
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew fetch`

Download bottles without installing.

```text
Download bottles without installing

Usage: zapbrew fetch [OPTIONS] <NAMES>...

Arguments:
  <NAMES>...  Formula or cask names to fetch

Options:
      --debug    Enable debug output
      --formula  Fetch formulae only [alias: --formulae]
      --cask     Fetch casks only [alias: --casks]
  -q, --quiet    Suppress non-essential output
      --deps     Also fetch the dependency closure
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew cleanup`

Remove stale downloads and old versions.

```text
Remove stale downloads and old versions

Usage: zapbrew cleanup [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names to clean; empty cleans all

Options:
      --debug         Enable debug output
  -s, --scrub         Scrub the download cache of the latest versions too
  -n, --dry-run       Show what would be removed without doing it
  -q, --quiet         Suppress non-essential output
      --prune <days>  Remove all cache files older than <days>, or 'all'
  -v, --verbose       Enable verbose output
      --prune-prefix  Only prune the symlinks and directories from the prefix
  -h, --help          Print help
```

### `zapbrew search`

Search for formulae and casks.

```text
Search for formulae and casks

Usage: zapbrew search [OPTIONS] <QUERY>

Arguments:
  <QUERY>  Search term, or `/regex/`

Options:
      --debug    Enable debug output
      --desc     Search descriptions as well as names
      --formula  Restrict the search to formulae
  -q, --quiet    Suppress non-essential output
      --cask     Restrict the search to casks
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew desc`

Show descriptions or search names/descriptions of formulae/casks.

```text
Show descriptions or search names/descriptions of formulae/casks

Usage: zapbrew desc [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula, cask, or search text

Options:
      --debug                                    Enable debug output
  -s, --search <SEARCH>                          Search names and descriptions for <text>
  -n, --search-name <SEARCH_NAME>                Search only names
  -q, --quiet                                    Suppress non-essential output
  -d, --search-description <SEARCH_DESCRIPTION>  Search only descriptions
  -v, --verbose                                  Enable verbose output
      --formula                                  Treat named arguments as formulae
      --cask                                     Treat named arguments as casks
  -h, --help                                     Print help
```

### `zapbrew postinstall`

Rerun post-install steps for installed formulae.

```text
Rerun post-install steps for installed formulae

Usage: zapbrew postinstall [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Formula names

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew config`

Show the effective configuration.

```text
Show the effective configuration

Usage: zapbrew config [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew shellenv`

Print shell integration for the environment.

```text
Print shell integration for the environment

Usage: zapbrew shellenv [OPTIONS] [SHELL]

Arguments:
  [SHELL]  Shell template: bash/sh, zsh, fish, csh/tcsh, or pwsh; other names use POSIX. Defaults to the current shell

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew tap`

Tap a formula repository, or list taps.

```text
Tap a formula repository, or list taps

Usage: zapbrew tap [OPTIONS] [NAME] [URL]

Arguments:
  [NAME]  Tap name, e.g. `user/repo`. Omit to list taps
  [URL]   Remote URL to clone the tap from

Options:
      --debug    Enable debug output
  -f, --force    Force the tap even for a built-in name
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew untap`

Remove a tapped repository.

```text
Remove a tapped repository

Usage: zapbrew untap [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Tap names to remove

Options:
      --debug    Enable debug output
  -f, --force    Remove even if formulae from the tap are installed
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew tap-info`

Show information about a tap.

```text
Show information about a tap

Usage: zapbrew tap-info [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Tap names to describe

Options:
      --debug      Enable debug output
      --installed  Show information for every installed tap
      --json       Emit JSON output
  -q, --quiet      Suppress non-essential output
  -v, --verbose    Enable verbose output
  -h, --help       Print help
```

### `zapbrew doctor`

Check the system for potential problems.

```text
Check the system for potential problems

Usage: zapbrew doctor [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew services`

Manage background services.

```text
Manage background services

Usage: zapbrew services [OPTIONS] [COMMAND]

Commands:
  list     List services and their status
  start    Start services
  stop     Stop services
  restart  Restart services
  run      Run a service without registering it to launch at login or boot
  info     Print one-line status for services
  kill     Stop a service but keep it registered
  cleanup  Remove unused service files
  help     Print this message or the help of the given subcommand(s)

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services list`

```text
List services and their status

Usage: zapbrew services list [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services start`

```text
Start services

Usage: zapbrew services start [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Service names to start

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services stop`

```text
Stop services

Usage: zapbrew services stop [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Service names to stop

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services restart`

```text
Restart services

Usage: zapbrew services restart [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Service names to restart

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services run`

```text
Run a service without registering it to launch at login or boot

Usage: zapbrew services run [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Service names to run

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services info`

```text
Print one-line status for services

Usage: zapbrew services info [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Service names to inspect

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services kill`

```text
Stop a service but keep it registered

Usage: zapbrew services kill [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Service names to kill

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew services cleanup`

```text
Remove unused service files

Usage: zapbrew services cleanup [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew update`

Fetch the latest catalog and taps.

```text
Fetch the latest catalog and taps

Usage: zapbrew update [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew shim`

Manage the opt-in `brew` shim.

```text
Manage the opt-in `brew` shim

Usage: zapbrew shim [OPTIONS] <COMMAND>

Commands:
  install  Install the `<prefix>/bin/brew` shim
  remove   Remove the `<prefix>/bin/brew` shim
  help     Print this message or the help of the given subcommand(s)

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew shim install`

```text
Install the `<prefix>/bin/brew` shim

Usage: zapbrew shim install [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew shim remove`

```text
Remove the `<prefix>/bin/brew` shim

Usage: zapbrew shim remove [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

### `zapbrew completions`

Manage shell completion links.

```text
Manage shell completion links

Usage: zapbrew completions [OPTIONS] [SHELL] [COMMAND]

Commands:
  state     Display the current completion link state
  link      Link discovered completion files into the active prefix
  unlink    Remove only links to discovered completion files
  generate  Generate a completion script from the live Clap surface
  help      Print this message or the help of the given subcommand(s)

Arguments:
  [SHELL]  Legacy shell shortcut, treated as `completions generate <shell>` when no subcommand is given [possible values: bash, zsh, fish]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew completions state`

```text
Display the current completion link state

Usage: zapbrew completions state [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew completions link`

```text
Link discovered completion files into the active prefix

Usage: zapbrew completions link [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew completions unlink`

```text
Remove only links to discovered completion files

Usage: zapbrew completions unlink [OPTIONS]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```

#### `zapbrew completions generate`

```text
Generate a completion script from the live Clap surface

Usage: zapbrew completions generate [OPTIONS] <SHELL>

Arguments:
  <SHELL>  Shell to generate a completion script for [possible values: bash, zsh, fish]

Options:
      --debug    Enable debug output
  -q, --quiet    Suppress non-essential output
  -v, --verbose  Enable verbose output
  -h, --help     Print help
```
