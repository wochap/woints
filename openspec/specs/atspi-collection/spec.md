# atspi-collection Specification

## Purpose
AT-SPI element collection strategies, extents resolution and phantom filtering for the focused frame.

## Requirements

### Requirement: Collection strategy
`CollectionStrategy` SHALL call `org.a11y.atspi.Collection.GetMatches` on the selected frame with a rule requiring all of SHOWING, VISIBLE and SENSITIVE and any of the roles push button, toggle button, link, entry, check box, radio button, combo box, menu item, check menu item, radio menu item, page tab, list item, tree item, spin button, slider, password text; sort order canonical, no count limit, traverse true.

#### Scenario: GTK3 widget factory
- **WHEN** the frame of `gtk3-widget-factory` is collected
- **THEN** `GetMatches` returns at least 30 objects in under 10 ms and the strategy reports `used = collection`

#### Scenario: Collection interface absent
- **WHEN** the frame's `GetInterfaces` lacks `org.a11y.atspi.Collection` (GTK4)
- **THEN** the provider skips directly to the tree walk without an error

#### Scenario: Collection returns nothing
- **WHEN** `GetMatches` returns zero objects
- **THEN** the provider falls back to the tree walk and records reason `collection-empty`

### Requirement: Bounded pipelined tree walk
`TreeWalkStrategy` SHALL traverse the frame breadth-first, reading children with up to 16 concurrent requests, honouring a node budget (default 1500), a depth cap (default 40) and the provider's time budget, and SHALL set `truncated = true` on the snapshot when any limit stops it. It SHALL keep only nodes whose role is interactive and whose state set contains SHOWING and VISIBLE.

#### Scenario: GTK4 calculator
- **WHEN** the `gnome-calculator` frame is walked
- **THEN** about 100 nodes are visited, at least 30 interactive elements are returned and `collect.match` is under 40 ms

#### Scenario: Budget exhausted
- **WHEN** a frame has more nodes than the budget
- **THEN** the walk stops, returns what it has and the snapshot carries `truncated = true`

### Requirement: Web content pruning
When the active toolkit profile sets `skip_web_content`, both strategies SHALL exclude the subtree of nodes whose numeric role is document web (95), document frame (82) or embedded (78).

#### Scenario: Firefox chrome only
- **WHEN** Firefox is collected with `skip_web_content = true`
- **THEN** no returned element has a `document web` ancestor and the walk visits fewer than 600 nodes

### Requirement: Window-relative extents to global coordinates
The provider SHALL request `Component.GetExtents` with `CoordType::WINDOW` for every candidate, pipelined, and convert to global coordinates by adding the target's global origin (M1 design D3). It SHALL NOT use `CoordType::SCREEN`.

#### Scenario: GTK4 element
- **WHEN** a GTK4 button reports WINDOW extents `(320, 301, 34, 34)` and the window is at `(100, 50)`
- **THEN** the element rect is `(420, 351, 34, 34)`

### Requirement: Geometry filters
The provider SHALL discard elements whose width or height is 0, whose rect lies outside the target rect, and whose rect is not contained in its parent's extents when the parent is a list or menu. For Qt it SHALL also discard list items whose nearest combo box ancestor lacks the EXPANDED state.

#### Scenario: Qt collapsed combo popup
- **WHEN** qt6ct reports list items at `(0, 0, 640, 31)` under a combo box that is not EXPANDED
- **THEN** those items are dropped and the combo box itself is kept

#### Scenario: Hidden zero-size item
- **WHEN** an element reports extents `(0, 0, 0, 0)`
- **THEN** it is dropped

### Requirement: Collection timing stages
The provider SHALL record `collect.connect`, `collect.match`, `collect.extents` and `collect.filter` durations in `Timings`.

#### Scenario: Timings present
- **WHEN** a `hints` request completes on a GTK3 app
- **THEN** `status.last_timings` contains all four `collect.*` stages and their sum is under 100 ms

### Requirement: Numeric role identification
The provider and actuator SHALL identify every node's role with `org.a11y.atspi.Accessible.GetRole` (numeric `AtspiRole`) and SHALL NOT call `GetRoleName` or `GetLocalizedRoleName` for any decision. The role string carried in `Element.role`, filter candidates and logs SHALL be the canonical AT-SPI name of that number, produced locally; an unknown number SHALL map to `unknown`.

#### Scenario: Toolkit reports an empty role name
- **WHEN** a WebKitGTK node returns `''` from `GetRoleName` and `43` from `GetRole`
- **THEN** it is treated as interactive and its element role is `push button`

#### Scenario: Toolkit reports a non-canonical role name
- **WHEN** a GTK 3.24 title-bar button returns `'button'` from `GetRoleName` and `43` from `GetRole`
- **THEN** the tree walk keeps it and its element role is `push button`

#### Scenario: Unknown role number
- **WHEN** a node returns a role number outside the AT-SPI enumeration
- **THEN** its role name is `unknown` and it is not interactive
