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
The protocol SHALL support `{"cmd":"act","snapshot_id":N,"id":E,"action":"activate|focus|left|right|middle|double|hover"}`. The daemon resolves the element from its snapshot store and invokes the actuator chain without showing the overlay. Unknown snapshot ids or element ids SHALL yield `ok:false`.

#### Scenario: Activate from snapshot
- **WHEN** `snapshot` returned id 17 with element 3 tagged `activate` and `{"cmd":"act","snapshot_id":17,"id":3,"action":"activate"}` arrives
- **THEN** the reply is `{"ok":true,"performed":"activate"}` and `DoAction` was called once

#### Scenario: Expired snapshot
- **WHEN** `act` references a snapshot id no longer in the store
- **THEN** the reply is `{"ok":false,"error":"unknown snapshot 17"}`

### Requirement: hints action parameter
`hints` SHALL accept `action` (default `activate`); the session uses it when a label is matched. In M2 only `activate` and `focus` succeed; others return `NeedsPointer`.

#### Scenario: Default action
- **WHEN** `{"cmd":"hints","scope":"focused"}` arrives
- **THEN** the session action is `activate`

### Requirement: CLI act
`woints act --snapshot N --id E [--action A]` SHALL map to the `act` command and print the result.

#### Scenario: CLI
- **WHEN** `woints act --snapshot 17 --id 3` runs
- **THEN** it sends the `act` request and exits 0 on `ok:true`
