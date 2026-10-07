# socket-protocol Specification

## Purpose
JSON-lines control socket protocol and CLI mapping.

## Requirements

### Requirement: JSON-lines transport
`wointsd` SHALL listen on `$XDG_RUNTIME_DIR/woints.sock`. Each request is one JSON object terminated by `\n`; each response is one JSON object terminated by `\n`. Requests carry `"cmd"`; responses carry either `"ok": true` plus payload or `"ok": false, "error": "<message>"`.

#### Scenario: Unknown command
- **WHEN** a client sends `{"cmd":"frobnicate"}`
- **THEN** the daemon replies `{"ok":false,"error":"unknown command: frobnicate"}` and keeps the connection usable

### Requirement: Commands
The protocol SHALL support: `hints` (`scope`, optional `action`), `cancel`, `status`, `quit`, `snapshot` (`scope`), `bench` (`iterations`, `scope`). In this milestone `hints` and `snapshot` produce demo elements (hardcoded boxes relative to the focused window) because no element provider exists yet.

#### Scenario: hints shows overlay
- **WHEN** `{"cmd":"hints","scope":"focused"}` is received while idle
- **THEN** the daemon replies `{"ok":true,"state":"showing","targets":1,"elements":N}` after the overlay has been committed

#### Scenario: hints while showing
- **WHEN** `hints` arrives while the overlay is already showing
- **THEN** the current session is cancelled and a new one starts

#### Scenario: cancel
- **WHEN** `{"cmd":"cancel"}` is received while showing
- **THEN** the overlay unmaps and the reply is `{"ok":true,"state":"idle"}`

#### Scenario: status
- **WHEN** `{"cmd":"status"}` is received
- **THEN** the reply includes `state`, `version`, `uptime_ms`, `outputs` (name, scale, logical rect) and `last_timings` (per-stage ms of the last `hints`)

#### Scenario: snapshot
- **WHEN** `{"cmd":"snapshot","scope":"focused"}` is received
- **THEN** the reply contains `snapshot_id`, `targets: [Target]` and `elements: [Element]` in global logical coordinates, without showing the overlay

#### Scenario: quit
- **WHEN** `{"cmd":"quit"}` is received
- **THEN** the daemon replies `{"ok":true}`, unmaps any overlay, removes the socket file and exits 0

### Requirement: CLI mapping
The `woints` binary SHALL map subcommands `hints [--scope S] [--action A]`, `cancel`, `status`, `quit`, `snapshot [--scope S] [--json]`, `bench [-n N]` one-to-one onto protocol commands, print the response (human-readable by default, raw JSON with `--json`) and exit non-zero when `ok` is false or the daemon is unreachable.

#### Scenario: Daemon not running
- **WHEN** `woints status` runs with no daemon
- **THEN** it prints `wointsd is not running (socket: <path>)` to stderr and exits 2

### Requirement: act command
The protocol SHALL support `{"cmd":"act","snapshot_id":N,"id":E,"action":A}` with `A` from the `action-modes` vocabulary. The daemon resolves the element from its snapshot store and invokes the actuator chain without showing the overlay. Unknown snapshot ids or element ids SHALL yield `ok:false`.

#### Scenario: Activate from snapshot
- **WHEN** `snapshot` returned id 17 with element 3 supporting `click` via AT-SPI and `{"cmd":"act","snapshot_id":17,"id":3,"action":"click"}` arrives
- **THEN** the reply is `{"ok":true,"performed":"click"}` and `DoAction` was called once

#### Scenario: Expired snapshot
- **WHEN** `act` references a snapshot id no longer in the store
- **THEN** the reply is `{"ok":false,"error":"unknown snapshot 17"}`

### Requirement: CLI act
`woints act --snapshot N --id E [--action A]` SHALL map to the `act` command and print the result.

#### Scenario: CLI
- **WHEN** `woints act --snapshot 17 --id 3` runs
- **THEN** it sends the `act` request and exits 0 on `ok:true`

### Requirement: Action parameter on hints and act
`hints` SHALL accept an optional `action` (one of the `action-modes` strings, default `click`). `act` SHALL accept `{"cmd":"act","snapshot_id":N,"id":E,"action":A}` and, for `drag`, an additional `"to": E2`; for `scroll` an additional `"axis":"vertical"|"horizontal","amount":<int lines>`. Unsupported combinations SHALL return an error naming the element's supported actions.

#### Scenario: act drag
- **WHEN** `{"cmd":"act","snapshot_id":7,"id":3,"action":"drag","to":9}` is received
- **THEN** the drag sequence from element 3 to element 9 runs without showing the overlay and the reply is `{"ok":true}`

#### Scenario: act unsupported
- **WHEN** `act` requests `focus` on a vision element
- **THEN** the reply is `{"ok":false,"error":"element 3 does not support focus; supported: click,right,middle,double,hover,drag,scroll"}`

#### Scenario: act scroll
- **WHEN** `{"cmd":"act","snapshot_id":7,"id":3,"action":"scroll","axis":"vertical","amount":-3}` is received
- **THEN** three lines of upward scroll are sent at element 3's centre

### Requirement: Snapshot lists element actions
Each element in a `snapshot` reply SHALL include `actions: [string]`.

#### Scenario: Snapshot element
- **WHEN** `snapshot` runs on a window with a GTK3 button
- **THEN** that element's JSON contains `"actions":["click","right","middle","double","hover","focus","drag","scroll","copy"]`

### Requirement: CLI --action
`woints hints` SHALL accept `--action <a>` and `woints act --snapshot N --id E --action A [--to E2] [--axis v|h --amount N]`, mapping one-to-one onto the protocol.

#### Scenario: CLI drag
- **WHEN** `woints hints --action drag` runs
- **THEN** the request sent is `{"cmd":"hints","scope":"<default>","action":"drag"}`

### Requirement: reload and doctor commands
The protocol SHALL add `reload` (reply `{"ok":true,"config":{...}}` or `{"ok":false,"error"}`) and `doctor` (reply `{"ok":true,"report":{...}}` with the grouped checks). `status` SHALL include `config: {path, source, loaded_at, errors[]}`.

#### Scenario: doctor over socket
- **WHEN** `{"cmd":"doctor"}` is received
- **THEN** the reply contains `report.environment`, `report.a11y`, `report.focused`, `report.wayland`, `report.quickshell`, `report.hyprland`, `report.config`

#### Scenario: status config
- **WHEN** `{"cmd":"status"}` is received after a failed reload
- **THEN** `config.errors` is non-empty and `config.loaded_at` is the time of the last successful load
