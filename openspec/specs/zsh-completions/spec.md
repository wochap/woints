# zsh-completions Specification

## Purpose
zsh completion script for the `woints` CLI, its installation, and a test that keeps it in sync with the clap command tree.

## Requirements

### Requirement: Subcommand completion
The zsh completion for `woints` SHALL offer every clap subcommand (`hints`, `cancel`, `status`, `quit`, `snapshot`, `act`, `bench`, `reload`, `doctor`, `help`) as the first positional word, each with the one-line description from its clap doc comment, and SHALL offer the global `--json`, `--help` and `--version` flags at that position.

#### Scenario: First word
- **WHEN** the user types `woints <TAB>`
- **THEN** zsh lists the ten subcommands with descriptions, plus `--json`, `--help`, `--version`

#### Scenario: Subcommand already given
- **WHEN** the user types `woints status <TAB>`
- **THEN** zsh offers only `--json` and `--help`, not another subcommand

### Requirement: Flag completion per subcommand
For a subcommand already on the line, the completion SHALL offer exactly that subcommand's long flags (and the `-n` short form of `bench --iterations`) plus the global `--json`, and SHALL NOT offer a flag that has already been given.

#### Scenario: act flags
- **WHEN** the user types `woints act --snapshot 7 --<TAB>`
- **THEN** zsh offers `--id`, `--action`, `--to`, `--axis`, `--amount`, `--json`, `--help` and not `--snapshot`

#### Scenario: hints flags
- **WHEN** the user types `woints hints --<TAB>`
- **THEN** zsh offers `--scope`, `--action`, `--json`, `--help`

### Requirement: Enum value completion
After `--scope`, `--action` or `--axis` (or their `=` forms) the completion SHALL offer the full value set accepted by the corresponding `FromStr`: scopes `focused`, `focused+layers`, `layers`, `all`; actions `click`, `right`, `middle`, `double`, `hover`, `focus`, `drag`, `scroll`, `copy`; axes `v`, `h`, `vertical`, `horizontal`. Each value SHALL carry a short description. Numeric flags (`--snapshot`, `--id`, `--to`, `--amount`, `-n/--iterations`) SHALL complete nothing but show a message naming the expected argument.

#### Scenario: Scope values
- **WHEN** the user types `woints hints --scope <TAB>`
- **THEN** zsh lists `focused`, `focused+layers`, `layers`, `all` with descriptions

#### Scenario: Action values with equals
- **WHEN** the user types `woints act --snapshot 7 --id 3 --action=<TAB>`
- **THEN** zsh lists the nine actions

### Requirement: Drift guard
`cargo test -p woints-cli` SHALL fail when the set of subcommands, long flags, short flags or enum values accepted by the clap `Command` tree differs from the set spelled out in `integrations/zsh/_woints`.

#### Scenario: New flag without completion
- **WHEN** a developer adds `#[arg(long)] limit: Option<u32>` to `Cmd::Snapshot` and does not edit `_woints`
- **THEN** the test fails naming `snapshot --limit` as missing from the completion file

#### Scenario: Stale flag in completion
- **WHEN** `_woints` lists a flag that no subcommand accepts
- **THEN** the test fails naming the extra flag

### Requirement: Installation
The Nix package SHALL install the completion file at `share/zsh/site-functions/_woints` for every `mkWoints` variant. The file SHALL start with `#compdef woints` so it also works when dropped into any `fpath` directory manually, and the README SHALL document both routes.

#### Scenario: Nix build
- **WHEN** `nix build` finishes
- **THEN** `result/share/zsh/site-functions/_woints` exists and its first line is `#compdef woints`

#### Scenario: Manual install
- **WHEN** the user copies `integrations/zsh/_woints` into a directory on `fpath` and runs `compinit`
- **THEN** `woints <TAB>` completes without any further configuration
