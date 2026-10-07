# vision-fallback-policy Specification

## Purpose
Per-target decision on when vision runs, its reason, and its time budget.

## Requirements

### Requirement: Policy inputs and modes
The pipeline SHALL decide per target whether to run vision from: `vision.mode ∈ { fallback, always, off }` (default `fallback`), the number of usable AT-SPI elements for the target, `vision.min_atspi_elements` (default 3), `vision.force_classes` (list of Hyprland classes, glob), the target kind, and whether any other provider served the target.

#### Scenario: Enough AT-SPI elements
- **WHEN** mode is `fallback` and AT-SPI returned 12 usable elements for a toplevel not in `force_classes`
- **THEN** vision does not run and `status.last_timings` has no `capture` stage for that request

#### Scenario: Too few AT-SPI elements
- **WHEN** mode is `fallback` and AT-SPI returned 2 usable elements
- **THEN** vision runs and the reason `too_few_atspi` is logged at debug level

#### Scenario: Forced class
- **WHEN** the target class matches `force_classes` entry `steam*`
- **THEN** vision runs regardless of the AT-SPI count

#### Scenario: Layer without provider
- **WHEN** the target is a `Layer` and no provider returned elements for it
- **THEN** vision runs with region capture

#### Scenario: Mode always
- **WHEN** mode is `always`
- **THEN** vision runs for every target and merge suppresses duplicates

#### Scenario: Mode off
- **WHEN** mode is `off`
- **THEN** vision never runs, even for forced classes

### Requirement: Decision exposed
The decision and its reason SHALL be returned in `snapshot` as `vision: { ran: bool, reason: string }` and printed by `woints snapshot`.

#### Scenario: Snapshot reason
- **WHEN** vision ran because of `too_few_atspi`
- **THEN** the snapshot JSON contains `"vision": {"ran": true, "reason": "too_few_atspi"}`

### Requirement: Budget
Vision SHALL respect a per-request budget `vision.budget_ms` (default 250). If capture plus detect exceed it, detection returns what it has so far (detector checks the deadline between stages) and the request still completes.

#### Scenario: Slow detection
- **WHEN** detection has not finished by the deadline
- **THEN** the pipeline proceeds with the partial detections and logs `vision_budget_exceeded`
