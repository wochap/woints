# daemon-lifecycle Specification

## Purpose
Single-instance daemon, session state machine, stage timing, bench and Nix packaging.

## Requirements

### Requirement: Single instance
`wointsd` SHALL refuse to start if another instance answers `status` on the socket; a stale socket file with no listener SHALL be removed and replaced.

#### Scenario: Stale socket
- **WHEN** the socket file exists but connecting fails with `ECONNREFUSED`
- **THEN** the daemon deletes it and binds successfully

#### Scenario: Live instance
- **WHEN** another daemon answers `status`
- **THEN** the new process prints `wointsd already running` and exits 1

### Requirement: Session state machine
The daemon SHALL hold one session state: `Idle` or `Showing { snapshot_id, typed: String }`, with transitions `hints→Showing`, `cancel|Esc|quit→Idle`. Every request is handled on the tokio runtime; the Wayland overlay runs on its own thread with `calloop`, bridged by channels.

#### Scenario: Overlay thread crash is contained
- **WHEN** the overlay thread panics
- **THEN** the daemon logs the panic, answers subsequent `hints` with `{"ok":false,"error":"overlay unavailable"}` and `status` still works

### Requirement: Stage timing
Every pipeline stage (`hypr.query`, `select`, `collect`, `label`, `place`, `render`, `commit`) SHALL log its duration at `debug` level and record it for `status.last_timings`.

#### Scenario: Debug log
- **WHEN** `RUST_LOG=debug` and a `hints` request completes
- **THEN** the log contains one `stage=<name> ms=<f32>` line per stage

### Requirement: Bench
`woints bench -n N` SHALL run `snapshot` plus render (without mapping the overlay) N times and print p50 and p95 per stage and total.

#### Scenario: Twenty iterations
- **WHEN** `woints bench -n 20` runs
- **THEN** output has one row per stage with `p50` and `p95` columns in milliseconds

### Requirement: Nix flake
The repository SHALL provide a `flake.nix` with `devShells.default` (Rust stable, pkg-config, wayland, libxkbcommon) and `packages.default` building both binaries.

#### Scenario: nix build
- **WHEN** `nix build` runs
- **THEN** `result/bin/woints` and `result/bin/wointsd` exist
