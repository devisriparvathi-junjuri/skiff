# Host API — M0 contract, local M1a–M1g seams, M2 remote terminals, M3 remote lifecycle, the M4 data plane (reads, then project and worktree writes), M5 files and the M6 ledger and Skipper reads

Tracking: [M0 #156](https://github.com/solutionscay/skiff/issues/156),
[epic #155](https://github.com/solutionscay/skiff/issues/155).
This is the repository copy of the proposed wiki contract page. It can be
published with the wiki glossary when the V2 docs are published; M0 does not
push to the separate wiki repository.

## Glossary

- **Host**: the machine/account that owns host-scoped data. Its opaque `HostId`
  is durable identity, never derived from a hostname, endpoint, socket or PID.
- **Endpoint**: a way to reach a Host. `Local { socket }` and
  `Ssh { hostname, account, port, socket }` are descriptors. The SSH variant has
  no implementation in M0. A Host may have more than one endpoint.
- **Selected Host**: the one Host whose data a future app window presents.
  Selection is not implemented in M0. No cross-Host aggregation is implied.
- **Capability**: `{ name, version }`, a feature's major contract. `sessions@1`
  is the M0 operation family; M1a adds `connection_status@1` and M1b adds
  read-only `session_screen@1`; M1c adds `session_inventory@1`; M1d adds read-only
  `session_updates@1` events; M1e adds `session_terminal@1` (attach, output,
  input, resize, detach of existing sessions); M1g adds `session_lifecycle@1`
  (create, kill, rename, theme, seen) and `session_groups@1` (list, save, delete).
  Unknown names/majors are retained as metadata and ignored unless an operation
  requires them.

The ownership rule is: if data would differ on another Host, it must eventually
come from that Host. Projects/fleets/worktrees, Git, files/images, sessions/groups,
ledger/results, Skipper, task types and agents are Host-owned. Keys, appearance,
window state, local Open with policy and workspace-profile files are client-owned.
M0 does not move any of them.

## Code seams and current behavior

`skiff-host-api` depends at runtime only on serde and base64. It has no I/O,
application state, runtime, core re-exports, config reader or process launcher.

`skiff-client::transport::Transport` splits an ordered byte stream into owned,
Send/Unpin asynchronous reader/writer halves. Its production implementation is
UnixStream and delegates directly to the existing `into_split()`. The existing
`Client::connect` still connects to the same socket, reads the same local peer
credentials and sends only the existing skiffd requests. Ping/Pong, PROTOCOL 27,
input/output queues, lag recovery and terminal framing are unchanged.

`skiff-client::host::HostConnection` is an opt-in M0 interface over Transport.
It has no concrete socket or process-ownership type in its public API. It
negotiates Hello, gates typed requests, correlates replies, offers an event
receiver, and can disconnect. The CLI and daemon do not use it. M1a integrates desktop connection status, M1b adds a text snapshot, and M1c
adds the desktop inventory read through the same local adapter, as described below.
A local UnixStream fixture and a fake duplex stream exercise the same interface.
M0 fixtures wrote no identity/config file; M1a provisions local bootstrap
identity as described below, without changing the legacy daemon protocol.

No `skiff-host` process, IPC proxy or extra listener is implemented. M1 decides
how the session/data planes are reached and introduces one HostConnection per
window; M2 introduces SSH. The proposed Host envelope below is a separate opt-in
contract, not a replacement accepted by today's skiffd socket.

## Schema, API major 1

Types in `crates/skiff-host-api/src/lib.rs` are the authoritative field list;
`tests/fixtures/m0.json` contains golden examples of every M0 variant.

- Host: `host_id` (opaque nonblank string), `os`, `account`.
- Endpoint: tagged `kind: local | ssh`, with the fields above. Paths are opaque
  strings on the named machine, never a command or an implicit local path.
- Hello: flattened Host fields, `instance_id` (opaque nonblank string),
  `service_versions` (name → version string), `api_major`, `capabilities`,
  `limits { max_frame_bytes, max_in_flight }`. Limits are positive integers;
  OS/account and capability names must be nonempty, capability versions nonzero,
  and duplicate capability/name-version pairs are malformed.
- Session: `id`, `cwd`, `label`, optional `generation`. No broad core SessionInfo
  dependency or new project/ledger schemas are introduced for future milestones.
- Envelope: `{ id, request }`; request is tagged `cmd` and is one of `hello`,
  `list_sessions`, `write { session, data, paste=false }`,
  `resize { session, cols, rows }`, `subscribe { session }`, `unsubscribe { session }`.
- Response: tagged `kind`: `hello { hello }`, `sessions { sessions }`, `ok`.
- Event: tagged `event`: `snapshot { session, data, cols, rows }`, `output { session, data }`,
  `session_changed { session }`. Snapshot dimensions and arbitrary bytes retain
  their session-plane meaning; this is not a new terminal implementation.
- Message: tagged `type`: `reply { id, result }` or `event { event }`. Result is
  exactly one of `{ "Ok": Response }` and `{ "Err": Error }`.

Control frames are newline-delimited UTF-8 JSON; each binary `data` is standard
base64, preserving arbitrary bytes including invalid UTF-8. Request IDs start
at 1 for a new connection, increase without reuse, and correlate replies even
when they arrive out of order. Events are ordered as received. Snapshot can
arrive before the subscribe reply: take the event receiver first.

M0 session operation requests require exactly `sessions@1`; Hello is handshake-only.
M1a `connection_status` requires exactly `connection_status@1` and replies with
`{ kind: "connection_status", version, protocol }`; these are the existing skiffd
service version and session-plane PROTOCOL, not Host API versions.
A reply of the wrong variant is malformed. Unknown optional fields are ignored;
required fields and unknown message variants must parse or fail explicitly.
Future operations/messages must be gated behind a capability they require.

## Compatibility and errors

API major 1 changes only for an envelope/handshake break. A breaking feature
change increments that capability's major; serve the previous major for at
least one release line. Additive optional metadata needs no major bump. The
existing session-plane PROTOCOL and release/service version strings are separate
from both numbers. The supported product window is the current and previous
minor release lines; fixtures will accumulate as those Host releases exist.
Within the same API major, service versions alone are diagnostic: older or newer
services can offer reduced capabilities, never a fallback to local data.

Unknown API major is `incompatible_version { supported, received }` before any
operation dispatch. Missing/wrong capability major is `unsupported_capability
{ capability }`, also before dispatch. Error is tagged `code`; other variants:

- `malformed_contract { message }`, `not_found { message }`, `conflict { message }`,
  `permission { message }`;
- `host_busy`, `transport_lost`, `cancelled`, `timed_out`;
- `identity_changed { expected, received }` (HostIds).

These are contracts, not new UI strings. M1 maps them to existing design rules.
M0 does not change the legacy Client's errors or UI surfaces.

## Identity, cancellation, bounds and reconnect

Validate Hello before making the HostConnection available. On an endpoint's
next connection, pass the previous Hello: changed host id rejects reuse; changed
instance id returns NewInstance, requiring fresh subscriptions and snapshots.
SameInstance still creates fresh transport subscriptions. Host provisioning will
create durable host IDs and new instance IDs on start/reload; M0 only defines
and tests this lifecycle. There is no automatic reconnect/resubscription.

The local safety bounds are 64 in-flight requests, a 64-frame write queue,
8 MiB per frame (including newline) and 256 queued events per receiver. Outgoing
limits use the smaller of these and the advertised Host limits. The request
budget defaults to 20 seconds, including queueing; test callers can set it.
Oversize/incomplete/invalid incoming frames fail the connection explicitly.
Control replies are processed without waiting for event consumers. A slow event
receiver receives Lagged and must discard backlog and resubscribe for a snapshot.

Dropping a request future cancels its local wait/removes its correlation slot.
Canceled queued writes are skipped when possible; an already in-flight operation
cannot be retracted, so cancellation/timeout does not promise it did not run.
Late/duplicate replies have no waiter and are ignored. Disconnect completes all
remaining waiters once and closes event receivers/tasks. There is no replay of
uncertain requests or keystrokes, including after identity/instance changes.
Disconnecting affects only the connection, never host sessions/processes.

## Enforced import boundary

Run `python3 scripts/host-boundary/check.py` and the adjacent unittest suite.
CI runs both. The contract's runtime dependencies are checked against the exact
serde/base64 allowlist, and its source is checked for I/O/application references.

The desktop check catches Rust host imports (including aliases, grouped uses,
re-exports and fully qualified paths), filesystem/process/environment/network
access, and TS direct invoke/network/Node/Tauri I/O imports. Executable template
interpolations are scanned recursively, including nested templates. Literal
relative TS paths are normalized (dot segments, explicit extensions, emitted
`.js` paths and index barrels). Local adapter dependencies propagate transitively
through callers/re-exports; Rust module names and root/ancestor references are
conservatively tracked without symbol resolution. Imports of the new Host
contract/client seam are allowed.

The temporary baseline inventories 77 existing adapters/callers plus both desktop
dependency manifests with exact SHA256 content and purpose labels. New dependency
aliases/providers also require review. It is deliberately conservative: even
adding a call through an existing alias changes the whole file's fingerprint and
fails. Type-only imports, inert relative-path strings and ambiguous Rust names
may over-report. There are no directory or wildcard exemptions.
Client policy files (appearance, keys/UI/native menus, clipboard, workspace files)
have explicit exact-file entries rather than broad immunity.

Do not casually regenerate the baseline. Inspect a changed file's ownership,
then explicitly review any remaining direct local accesses and their purpose
before updating its fingerprint with `--record`. A file with no remaining direct
or transitive access must have its entry removed. M1–M6 progressively move providers into a
local adapter and shrink the inventory; graduation requires no legacy host-data
entries. This is a lexical architecture tripwire, not a malicious-code sandbox
or full Rust/TS semantic resolver: computed module specifiers, custom resolver
aliases, macro expansion and reflective/dynamically constructed I/O are not
resolved. Regex recognition inside template expressions uses a lexical heuristic.
Baseline and checker changes require review.

## M1a: selected local Host owns connection status

The desktop's current single-window `App` runtime owns one fixed `LocalSelection`
and one active `SelectedLocalHost`. Only the selection can create its pending
client, and only that pending client can activate the Host. The accepted legacy
`Arc<Client>` is the exact same allocation lent to existing terminal/data callers
and the nonblocking input path; there is no second socket or terminal pipeline.
Initial connect, restart probe and reload reconnect obtain clients from this
selection. Existing compatibility, spawn/reload, CLI and watcher policy is kept.
This is **not** full M1 endpoint-policy/capability adoption.

The vertical slice is the existing `daemon_status` Ping/Pong refresh. It now
requests `connection_status@1` through `HostConnection`; the in-process local
`StatusSource` delegates to `Client::hello()` on that same selected client. No
Host envelope or Hello is sent to legacy skiffd. No listener, proxy, Host service,
worker process or extra round trip is added. The command's JSON shape, timeout,
warning logic and visible GUI flow stay unchanged. The broad session plane and event
streams are deliberately not advertised by this adapter. M1b below adds one
independent read; other session calls still use the legacy client. Missing/wrong capability major and unknown API
major are typed errors before source dispatch, never fallback to another client.
Local status requests have a 20-second limit, at most 64 in flight, cancellation
by dropping the future, and disconnect wakes pending calls without replay.

Local bootstrap uses an opaque random UUID in `local-host-id` beside the already
selected `projects.toml`. This is additive identity metadata, not daemon settings;
no existing config, state or ledger bytes move or change. Existing private-state
selection and `SKIFF_CONFIG` apply, so test/debug identities stay isolated. File
creation uses private `0600` permissions, a complete synced temporary file and
atomic no-clobber publication so concurrent app launches agree. Existing corrupt,
public, symlink or foreign-owned identity files fail explicitly, without silently
replacing identity or falling back to a path/PID-derived value. Bootstrap I/O runs
off async workers. It happens only after existing daemon compatibility policy
accepts a connection. An identity change within this selection rejects activation.

The UUID is durable for this local bootstrap store. `instance_id` is a fresh
**adapter connection generation**, not an authenticated daemon identity or PID;
reconnect conservatively yields NewInstance. (M1f: skiffd now serves this
identity itself; the bootstrap path remains only for older daemons.) Metadata advertises
`local-status-adapter` as its service, never a nonexistent `skiff-host`. Authoritative
daemon/service Hello rollout remains a later step; the future Host service must
adopt this identity deliberately. Copying identity metadata to another machine
is not supported provisioning or authentication.

Tests exercise the real desktop status entry point against a private fake daemon,
assert exact Ping IDs and shared Client identity, retain config/ledger bytes, and
verify reuse without another connection. Separate fixtures prove only the selected
endpoint receives status traffic, durable identity/concurrent bootstrap, explicit
capability/version/identity failures, cancellation/bounds and unchanged terminal
wire bytes. `local_host` is included in the existing boundary CI job.

Next deliberately excluded step: move another operation family (and its capability
checks) through the selected connection, with endpoint-local lifecycle guards and
authoritative Hello rollout designed before any remote endpoint. No SSH, host
picker, skiff-host service, migrations, data relocation or broad data conversion is
part of M1a. Per-surface capability UI and session event routing remain excluded.

## M1b: one read-only session text snapshot

The chosen operation is the existing Mission control `session_screen` command,
not session inventory. Inventory would require expanding the M0 session projection
to preserve the GUI's full SessionInfo; the text snapshot already returns only
`Vec<String>`. No UI source, screen rendering, command arguments or JSON result
shape changes. The desktop command calls `connection::screen_now`, obtains the
same selected owner via `ensure_host`, and requests the snapshot through that
owner's HostConnection. Its status and screen sources capture the very same
legacy `Arc<Client>`, created only by the fixed LocalSelection.

Additive API-major-1 contract:

- Capability: `session_screen@1`, independent of broad `sessions@1`.
- Request: `{ cmd: "session_screen", session, rows }` (`rows: u16`).
- Reply: `{ kind: "session_screen", lines: string[] }`.
- `operation_failed { message }`: an opaque legacy service rejection, with no
  invented not-found/permission classification. This error is only added for
  the new capability. Existing M0/M1a shapes/versions remain unchanged.

The in-process source sends exactly the existing `{ id, cmd: "screen", session,
rows }` to skiffd on the selected Client. The daemon's existing text extraction,
line order, empty lines, Unicode and whitespace are passed through unmodified;
zero and maximum u16 row counts are forwarded without client-side clamping.
No Host envelope, extra handshake, subscription, connection or request is added
to skiffd's wire protocol. Session inventory remains on its existing path.

Version/capability validation occurs before source dispatch; unknown API major,
missing capability and wrong capability major fail explicitly, without fallback.
The local adapter advertises only status plus this read-only capability, never
`sessions@1`. Mutations/lifecycle and event subscriptions stay unsupported by the
local Host contract. Both the local fake and negotiated transport test the new
request/reply family.

The temporary `legacy_screen` compatibility projection gives the existing Tauri
caller exactly its old error text: service messages (including older-daemon
"unknown variant" replies), unexpected reply diagnostics, connection closed and
the existing 20-second timeout text. Internally, stalls are typed TimedOut,
disconnect is TransportLost, malformed replies are MalformedContract and opaque
service rejections are OperationFailed. It never retries via a raw Client.

Mission control fans out reads for every visible tile. Snapshot requests wait
for a Host slot within their existing 20-second budget rather than acquiring a
slot with a new HostBusy failure on boards above 64 tiles. Drop/cancellation
releases a slot; disconnect wakes active and queued reads without replay. One
slot is reserved for status on the production 64-slot connection so a full board
cannot exhaust the status refresh quota. M1a status admission behavior is
retained. Raw terminal I/O and subscriptions
remain untouched.

Evidence: golden M1b types, fake gating/error/cancellation/deadline and 65-read
fanout fixtures, negotiated-transport coverage, and a private selected-socket
fixture comparing legacy and Host requests/results/errors on the exact same
Client. The desktop entry-point fixture checks both returned text and the
older-daemon error on the same accepted status connection. Existing M0/M1a and
bootstrap regression fixtures stay enabled in CI.

M1b deliberately deferred inventory until a lossless GUI metadata schema was
specified; M1c below supplies it. Terminal snapshots/events, mutation/lifecycle
and endpoint lifecycle policy still require their own increments. No SSH/remote transport, picker, service,
projects/files/ledger/Git movement, migrations or broad GUI conversion is part
of M1b. Bootstrap identity remains local adapter identity as described in M1a;
this slice does not turn legacy skiffd into an authoritative Host service.

## M1c: lossless desktop session inventory read

Only the existing Tauri `list_sessions` read now calls `inventory_now`, then
`legacy_inventory` on the same selected local HostConnection. Internal daemon
compatibility/reload checks still use their original legacy Client calls, avoiding
any bootstrap dependency cycle. No UI source, session mutation, event subscription
or other command path is converted.

Additive API-major-1 capability `session_inventory@1` has request
`{ cmd: "session_inventory" }` and response `{ kind: "session_inventory", sessions }`.
It is independent of `sessions@1`; M0 ListSessions/Session remains unchanged, and
local mutation/subscription operations are still unsupported. An InventorySource
on the same LegacyReadOnly/Arc<Client> sends exactly one existing
`{ id, cmd: "list_sessions" }` frame to skiffd. It neither chooses another endpoint
nor falls back to raw local access when capability/version validation fails.

`skiff-host-api::inventory::InventorySession` is deliberately only a read view of
today's desktop SessionInfo. It owns primitive/nested types, has no core re-export
or dependency, no SessionSpec, and no I/O. Its complete current field list is:

- Identity/display/program: `id`, `label`, `role`, `cwd`, `command`,
  `foreground_program`, `args`, `name`, `title`, `theme`.
- Observed session state: `state`, `cols`, `rows`, `pid`, `exit_code`,
  `started_at`, `last_output_at`, `unread`, `agent_exit`, `peek`, `interactive`,
  `input_gen`, `coordinator`.
- Existing read metadata: `away` (`path, command, at`), `was`/`resume`
  (`agent, title`), `watch` (`since, pids, failure, follows`), with watch failure
  (`kind, message, at`). These are metadata already in the session list, not
  project, filesystem, ledger or watcher-service APIs.

Role/state enums retain their existing four lowercase variants each. Integer
widths/signedness, vector ordering and duplicates, empty strings, zero values and
all 27 fields are preserved. Paths are opaque UTF-8 host strings with no filesystem
lookup, normalization or lossy conversion. Paths decoded from the legacy JSON wire
are UTF-8; an out-of-domain non-UTF-8 core path is explicitly rejected.

Serde defaults and null/omission rules match legacy output: false coordinator and
absent watch are omitted; nullable display/history fields remain null; missing
old-daemon counters/flags/lists get the same defaults; watch follows None means
unknown (omitted), while Some(empty) remains an empty array. Unknown source fields
are ignored by the existing core decoder as before; the view is lossless for the
current desktop model, not an arbitrary future JSON extension bag.

The adapter maps core → API → core explicitly, retaining the existing Tauri
`Vec<SessionInfo>` result and JSON shape. Struct destructuring/initializers and
enum matches are exhaustive, without `..`/wildcard fallbacks, so added core/API
fields or variants require deliberate projection review at compile time. Golden
fixtures plus legacy-vs-Host JSON equality cover full metadata, minimal old-daemon
data, all role/state combinations, extrema and nested optional semantics.

The operation shares the bounded read lane/20-second budget with screens, waits
rather than producing new HostBusy failures, and retains the status reservation.
Capability/version rejection happens before dispatch. Decode provenance is retained
through request_decoded: correlated malformed rows/nested data are MalformedContract,
genuine daemon Error is OperationFailed even with identical diagnostic text, and
stalls/disconnect are TimedOut/TransportLost. The compatibility helper preserves
legacy error text. No retry, input or request replay is introduced.

Tests include fake validation/ordering/metadata/deadline/cancellation, negotiated
Host transport, and a real selected-socket comparison of exact legacy frames,
ordering, duplicates, all fields and serialized output. They also cover malformed
inventory, genuine service errors with decode-like messages, a withheld reply,
malformed reply followed by EOF and an unused second endpoint. The real desktop
read entry point verifies the full result and service error on the same connection
as status/screen. Existing M0/M1a/M1b regressions remain in the boundary CI job.

Next excluded boundary: live session events/subscriptions still use the legacy
stream and need their own scoped contract/continuity work; inventory adoption does
not authorize mutation/lifecycle actions. Endpoint lifecycle guards, authoritative
Host Hello, SSH/remote selection/service, data conversion and migrations remain
outside M1c. Local bootstrap/adapter-generation identity limitations are unchanged.

## M1d: one read-only session-update stream

The chosen path is the existing Tauri `subscribe_events` stream: every non-output
skiffd event (state, away, title, exit, created, turn ended, updated, removed,
groups changed) that the page applies to its session list. It now calls
`updates_now`, which obtains the same selected owner via `ensure_host` and takes
`legacy_updates` from that owner's HostConnection. Terminal output/snapshots,
`subscribe_output`, input and every mutation/lifecycle path stay on the legacy
Client. The Tauri command name, argument, channel JSON, task replacement and the
page's own resubscribe/refresh policy are unchanged; no UI source changes.

Additive API-major-1 contract, explicit at each layer:

- Capability: `session_updates@1`, independent of `sessions@1`. It grants no
  request, output, input or lifecycle operation.
- Envelope: the existing `Message::Event`; new `Event` variant
  `{ event: "session_update", update }`. `Event::capability()` names the family's
  capability (`session_updates@1` here, `sessions@1` for the M0 events).
- Payload `SessionUpdate`, tagged `kind`: `state { session, state }`,
  `away { session, away[] }`, `title { session, title|null }`,
  `exit { session, code|null }`, `session_created { session }`,
  `turn_ended { session }`, `session_updated { session }`,
  `session_removed { session }`, `groups_changed`. Created/updated rows reuse the
  lossless M1c `InventorySession`; state/away reuse its enums/paths.
- No subscribe/unsubscribe request, acknowledgement or sequence number: a Host
  pushes the family to each connection that negotiated it, as skiffd does today.

`HostConnection::session_updates()` checks the capability first on both backends
and never falls back; an adapter without an `UpdateSource` cannot advertise it.
The local source wraps a receiver of the very same `Client::events()` broadcast the
desktop read directly before, created synchronously when the command runs, so the
page's "events first, then list" ordering holds. No frame is sent to skiffd. The
Client still routes output elsewhere and skips unreadable/unknown frames, so they
neither reach nor end the stream. Core → API → core mapping is exhaustive (no
wildcards); output/snapshot can never be projected as an update.

Receivers report `UpdateGap::Lagged(n)` (nothing replayed), `Malformed` (one
unprojectable event skipped, e.g. a non-UTF-8 path, unreachable from the JSON wire)
and `Closed`. The desktop compatibility receiver skips the first two exactly as the
old loop ignored Lagged, and ends on Closed. Local streams close on `disconnect()`
or when the connection is dropped, even with updates queued. Before M1d a dead
connection's task idled until the page replaced it; now it ends when reconnect,
restart or reload drops the selected owner. In both cases nothing more could
arrive and the page subscribes again as before. On the wire backend, malformed
frames fail the connection (existing rule) and the stream closes; other event
families are skipped by this receiver.

Tests: golden `m1d.json` (every kind, nulls/empties, rich and minimal rows,
malformed kinds/states), fake capability/source gating, ordering, gap reporting,
disconnect/drop closure, negotiated-transport gating/filtering/closure, and a
selected private socket comparing the legacy receiver and the Host stream on one
Client with interleaved output and unreadable frames and no outgoing frame. The
desktop entry-point fixture checks `updates_now` on the same status/screen/inventory
connection and its closure when the owner is dropped.

Next excluded boundary: terminal output/snapshot streaming (`subscribe_output`,
lag/resubscribe flow control; moved in M1e below) and all mutation/lifecycle
actions remain on the legacy Client and need their own scoped contracts. No SSH/remote transport,
picker, Host service, authoritative Hello, data migration or projects/files/
ledger/Git conversion is part of M1d.

## M1e: the session terminal through the selected Host

The existing desktop terminal path now reaches skiffd only through the selected
local `HostConnection`: `subscribe_output` (attach, snapshot, output, lag and
resubscribe), `unsubscribe_output`, `pty_resize`, and every keystroke/paste from
`pty_write` via the input bridge, including held input released after reload
or first connect. `App::live` now holds the selected HostConnection, not a raw
Client. The streaming loop, flow control (`BEHIND_HIGH`/`BEHIND_LOW`, acks,
64 KiB pieces), page channel bytes, echo JSON for the latency trace, Tauri
command names/arguments and UI source are unchanged. Session lifecycle and
mutations (create, kill, rename, theme, seen, coordinator), Skipper, config,
ledger and Git remain on the legacy Client.

Additive API-major-1 contract (`skiff-host-api::terminal`):

- Capability `session_terminal@1`, independent of `sessions@1`; it grants no
  create/kill/rename or inventory. M0 `write`/`resize`/`subscribe`/`unsubscribe`
  and `snapshot`/`output` stay `sessions@1` and unchanged.
- Requests, each answered `ok`: `terminal_attach { session }` (subscribe; a
  snapshot precedes later output and can arrive before the reply),
  `terminal_detach { session }`, `terminal_resize { session, cols, rows }`,
  `terminal_input { session, data, paste=false }` (base64 bytes).
- Events: `terminal_snapshot { session, data, cols, rows }` and
  `terminal_output { session, data, echo? }`; `echo { wrote, read }` is optional
  diagnostic metadata. `Event::into_terminal()` yields `TerminalFrame`.
- `Endpoint::is_local()` names the only endpoint kind the app may manage.

`HostConnection::terminal_output(session)` returns a receiver; take it before
attach. `terminal_input` is synchronous: it queues without waiting for a reply,
so calls from one thread keep order, and an error means this input was not
sent. Both check the capability first and never fall back to a raw Client.
`TerminalOutput::recv` reports `OutputGap::Lagged` (frames dropped: clear,
attach again, skip output until the new snapshot; nothing replayed) and
`OutputGap::Closed`. `HostConnection::is_closed()` is a no-round-trip liveness
check; the local adapter asks its source.

Local adapter: `LegacyTerminal` over the same selected `Arc<Client>` sends
exactly the legacy frames (`subscribe`, `unsubscribe`, `resize`, `write`, ids
from the same counter) and uses the existing per-session output queue (4096
chunks, lag flag, receiver replaced per session, closed on unsubscribe/EOF).
Input keeps the unbounded legacy queue so a keystroke never waits. Attach,
detach and resize have their own 64-slot lane within the 20-second budget, apart
from reads and the status reservation. Local receivers also close when the
selected owner is dropped (reconnect, restart, reload). Legacy error text is
kept by `legacy_attach/detach/resize/input`.

Wire backend: attach/detach/resize are ordinary correlated requests. Input is
written in call order on the bounded 64-frame write queue, is not cancellable
and has no waiter; a full queue refuses it with `host_busy` rather than queueing
for retry. Terminal events share the 256-event receiver bound; a slow receiver
reports Lagged. Whether input on a slow remote link should block, drop or turn
input off is an M2 decision (epic §4: input off while disconnected).

Reconnect and no replay: the desktop's existing policy is unchanged. Input typed
while a reload runs or before a connection exists is held (never sent, so never
uncertain) and released once onto the new connection only for sessions proven
to be the same child; anything already queued on a lost connection is gone, and
nothing is resent. After a lost connection, the page resubscribes and draws from
a fresh snapshot. Local continuity is always conservatively NewInstance (see
below), so every reconnect takes fresh subscriptions and snapshots.

Endpoint-local lifecycle guards: `ProcessControl` can only be made from a local
`Endpoint` (`LocalSelection::process_control`, `SelectedLocalHost::process_control`).
Daemon spawn in connect, SIGTERM/SIGKILL in compatibility replacement and in
restart, and the reload path now require it; an SSH descriptor never yields one.
CLI link reconciliation (`cli_path::sync`) and Skipper watcher reconciliation
(`skiff_agent::ensure_watch`) are not reached by the terminal boundary; M1f
below guards them.

Authoritative identity prerequisite: the local Hello is still synthesized by the
adapter (bootstrap host id file, adapter-generation instance id). skiffd has no
authoritative instance/generation in its Ping reply, so SameInstance can never be
proven locally, and a forwarded socket's peer credentials must never stand in for
it. Before any remote terminal (M2), a Host service or skiffd must send a real
Hello with a durable host id and a start/reload generation; this slice does not
fabricate one. M1f below supplies it from skiffd.

Tests: golden `m1e.json` (requests, events, echo extrema, legacy frame map),
fake capability/source gating, ordering, lag/clear, disconnect/drop/lost-source
closure and no replay; negotiated-transport gating, session filtering, input
order without waiting, ignored input replies, lag and disconnect; a selected
private socket comparing exact legacy frames for attach/input/paste/resize/
detach on one Client, a 5000-chunk flood reporting lag once, clear and EOF;
the desktop entry point for terminal attach/output/echo/input/resize/detach on
the same status/screen/inventory/updates connection; and the existing real
skiffd reload fixture (`held_input_survives_a_reload`) now typing through the
Host input path.

## M1f: service-provided Host identity and the remaining lifecycle guards

### Topology decision

The epic leaves open how the app reaches the session plane (skiffd) and the
future data plane (`skiff-host`): one socket where `skiff-host` proxies to
skiffd, or one SSH session carrying a forward to each. M1 decides for the
session plane now. **skiffd serves its own Host identity, and the app reaches
it directly; there is no proxy, Host process, listener or wrapper.** When a
data-plane service exists, it is a second endpoint of the same Host. It must
report the same `host_id` from the same store, and the app's one
HostConnection checks that both agree. It has its own `instance_id`, because it
restarts on its own schedule.

Tradeoff:
- A proxy would put a second process on every keystroke and output byte, the
  latency-critical path that #10/#12/#15 measure.
- With a proxy, a restart of the data plane would drop every terminal
  connection, which is the #62 failure the two-process split exists to avoid.
- The cost is two forwards and two handshakes per remote Host in M2, and a
  rule that their host ids must match.
- This was decided structurally, not measured. No data-plane service exists to
  put behind a proxy, and the direct path is the one already measured in
  production. M2 should still take RTT numbers over a real forward before the
  data plane lands.

### Authoritative identity

- `pong` gains one optional field, `identity { host_id, instance_id, os, account }`:
  - an older client ignores it
  - an older daemon omits it
  - `PROTOCOL` stays 27, under its documented rule for optional fields
  - the `pong` JSON of a daemon without it is unchanged
- **Durable id.** skiffd reads `host_id` from `local-host-id` beside its
  selected `projects.toml` (`skiff_core::host_identity`):
  - This is the same file, with the same private `0600`, no-clobber, no-symlink
    and refuse-to-replace rules, that the M1a desktop bootstrap wrote. The rules
    moved from `skiff-client` into `skiff-core`.
  - The Host now owns the id an existing install already has. Nothing is
    regenerated or migrated.
  - Private test/debug state keeps its own id, because the path follows the
    socket's private state and `SKIFF_CONFIG`.
- **Instance id.** `instance_id` is a random UUID made once per process image,
  after the socket is known. A reload execs a new image, so every start and
  every reload is a new instance, while the pid and sessions stay. It is never
  a pid, socket path or peer credential.
- **`os` and `account`** are skiffd's own (`uid:<euid>` of the daemon). A remote
  Host reports its own account, not the app's.
- **If skiffd cannot read its id** (corrupt or foreign file), it logs, omits
  `identity` and keeps serving sessions.

### Adapter and reconnect

- `PendingLocalConnection::daemon_info()` records the `pong` it reads.
  `activate()` sends nothing itself and adopts that pong's identity:
  `IdentitySource::Host`, with `service_versions` naming `skiffd`. It does not
  read the app's bootstrap file.
- The desktop's existing pre-activation Ping is that recorded `pong`, both on
  connect (`ensure_host`, then `check_info`) and on reload reconnect, so
  activation costs no extra round trip.
- **Older skiffd.** When there is no identity, or no recorded `pong`, activation
  uses the M1a bootstrap (`IdentitySource::LocalBootstrap`). Its adapter
  generation makes every reconnect NewInstance. After a reload onto an M1f
  skiffd, the same store yields the same `host_id` and continuity is
  NewInstance.
- **Validation on each activation:**
  - a changed `host_id` is `identity_changed` and the activation is rejected
  - a blank id is `malformed_contract`
  - the same `instance_id` is SameInstance
  - a new one is NewInstance
- **Subscriptions and snapshots are renewed on every reconnect, whatever the
  continuity.** A subscription belongs to a connection: dropping the selected
  owner closes its receivers (M1e), and the page attaches again on the new
  connection with a fresh `subscribe` and snapshot. SameInstance only says that
  the sessions are the same run. It never replays input or reuses a stream.
- **M2 requirement.** M2 must require `IdentitySource::Host` for any non-local
  endpoint, because a bootstrap file on this Mac says nothing about a remote
  Host.

### Endpoint-local lifecycle guards, completed

- `cli_path::sync(&ProcessControl)` runs at launch only when the selected
  endpoint grants one.
- `skiff_agent::ensure_watch(app, &ProcessControl, …)` starts or replaces
  `skiff watch` only with a token. `start_skiff_watch` and `open_skiff_agent`
  skip it without one. The watcher's socket is now the selected endpoint's,
  not a recomputed path.
- Together with M1e (spawn, stop/replace, restart, reload), every lifecycle
  action that M1 reaches now requires a token that only a local `Endpoint`
  yields.
- Left unguarded, deliberately: the explicit `install_cli` palette command (a
  user action on this Mac), the Skipper skill copy (`skiff_agent::sync`) and the
  rest of the Skipper folder, which move with the M6 Skipper APIs.

### Tests

- `skiff-core`: `pong` with and without `identity` (old JSON decodes, new
  serializes exactly); concurrent first publication, `0600`, public and corrupt
  files refused and kept.
- `skiff-client/tests/host_identity.rs`, fake peer:
  - adopted identity with the peer's own os/account, no extra frame after the
    recorded Ping, no bootstrap file, nothing from the pid
  - SameInstance, then NewInstance, then IdentityChanged, then blank refused,
    then the original Host accepted again
  - an older daemon gets the bootstrap identity and NewInstance; a reload onto a
    reporting daemon with the same store carries on
  - SameInstance still closes the old receiver and sends a new `subscribe` with
    a fresh snapshot
- `skiffd/tests/identity.rs`, real private daemon:
  - the id comes from its private store, `0600`
  - stable within a run; the adapter adopts it, and a reconnect is SameInstance
  - a reload keeps the pid and Host and gives a new instance
  - a restart keeps the Host and gives a new instance
  - a replaced store is refused on reconnect
- Desktop `held_input_survives_a_reload` (real skiffd): the connect Hello is
  `Host`; after the reload the `host_id` is the same, the instance is new and
  continuity is NewInstance. The watcher fixture runs with the local token.
- No live daemon, config or user socket is used.

### Not in M1f

- SSH transport, host picker, a `skiff-host` service, projects/files/ledger/Git
  movement, migrations, capability-driven UI, and lifecycle/mutation routing.
- The local adapter remains a Host contract in front of skiffd's legacy wire. It
  is not a Host envelope on the socket.

## M1g: session lifecycle and groups, and capability-gated surfaces

### What moved

These Tauri commands now reach skiffd only through the selected
`HostConnection`, behind two new capabilities:

- `session_lifecycle@1`: `create_session`, `kill_session`, `rename_session`,
  `set_session_theme`, `session_seen`. The Skipper folder change also ends
  Skipper's session through it.
- `session_groups@1`: `list_groups`, `save_group`, `delete_group`.

`remove_worktree`'s live-session check and the watcher's watch-pid read now use
the M1c inventory, not a raw `list_sessions`.

The Tauri command names, arguments, result JSON, legacy error text,
confirmations and UI flows are unchanged. The page still asks before End,
Kill all, Remove and Close, exactly as before.

### Contract (additive, API major 1)

- Requests:
  - `session_create { spec }` answers `session { session }`
  - `session_kill { session }` answers `ok`
  - `session_rename { session, name }` answers `session`
  - `session_set_theme { session, theme|null }` answers `session`
  - `session_seen { session }` answers `ok`
  - `group_list` answers `groups { groups }`
  - `group_save { group }` answers `group { group }`
  - `group_delete { group }` answers `ok`
- A reply of any other kind is `malformed_contract`.
- `SessionSpec` (`skiff-host-api::lifecycle`) mirrors the desktop spec:
  - It keeps the same defaults: shell role, 120×40, no peek, null name/cwd/command.
  - Paths are opaque UTF-8 host strings.
  - It has no coordinator flag. That flag is never on the legacy wire, and a core
    spec that sets it is refused before sending.
- `Group`, `Layout` and `SplitDir` (`skiff-host-api::groups`) keep the existing
  JSON. Because of the `f32` split ratio, `Request`, `Envelope`, `Response` and
  `Message` are now `PartialEq` without `Eq`.
- Session rows reuse the lossless M1c `InventorySession`.
- Mapping is exhaustive both ways (`skiff-client::host::lifecycle`), as in M1c.
- Golden fixture: `tests/fixtures/m1g.json`, including the legacy frame map.

### Local adapter

- `LegacySessions` on the same selected `Arc<Client>` sends exactly the old
  frames: `create_session`, `kill`, `rename_session`, `set_session_theme`,
  `seen`, `list_groups`, `save_group`, `delete_group`.
- **Errors keep their provenance:**
  - a daemon error is `operation_failed`
  - a decode failure is `malformed_contract`
  - a stall is `timed_out`
  - a lost connection is `transport_lost`
- The `legacy_*` helpers in `host::local` give each Tauri command its old
  result and error text.
- **Lane:** both families use the bounded read lane within the 20-second budget,
  and the status reservation stays. A project close that ends many sessions at
  once waits for slots rather than failing with `host_busy`.
- **No retry:** nothing is retried. A timed-out create or kill may have run.
- **No fallback:** a missing or wrong-major capability is refused before any
  frame, and an adapter cannot advertise a family it has no source for.

### Capabilities in the GUI: one mechanism

- `host_capabilities` returns the selected Host's `name@major` list.
- **`app/hostCaps.ts`** is the page's only path for session and terminal
  commands. It covers inventory, events, screen, attach/output/ack/input/resize/
  detach, lifecycle and groups.
  - `hostInvoke(cap, cmd, args)` refuses a command whose capability the Host
    does not advertise ("This Host does not offer …"), before any IPC.
  - Once the capabilities are known it calls `invoke` synchronously, so
    keystrokes keep their order.
  - The first call waits for the first answer.
- **`hostCan(cap)` decides whether a surface offers its action.** Unavailable
  actions are greyed in the menu bar and left out of the palette. This covers
  New session, Rename, Session theme, End session, Add pane, Remove from group,
  Mission control and the context menus (session, pane, worktree, project,
  group, selection, Kill all, Ungroup, Group theme).
- **Load points:** capabilities load at launch before the event stream, and
  again after a daemon reload.
- **Before the first answer, surfaces stay as they are.** The call itself still
  waits and checks.
- **Rust checks again on every request,** so a page bug cannot bypass a
  capability.
- **Local behavior:** the local Host advertises every family, so nothing visible
  changes.

### Audit while integrating

- **Lifecycle tokens.** `reload::plan` ran the bundled `skiffd --reload-info`
  from the daemon check without the endpoint-local token. It now takes
  `ProcessControl`, and the check plans no reload without one.
- Every desktop action that spawns, signals, reloads, links or replaces a Host
  process now requires the token:
  - daemon spawn
  - compatibility stop/replace
  - restart
  - reload plan and run
  - CLI link
  - watcher start/replace
- `install_cli` (an explicit user action on this Mac) and the memory diagnostics
  `ps` read of this Mac's processes are not Host lifecycle actions.
- **Identity and reconnect** (M1f) were re-checked on the paths M1g touches. No
  defect was found:
  - a refused identity leaves the previous Hello in place, so a changed Host
    stays refused
  - restart signals only a local peer pid under the token
  - SameInstance still renews subscriptions, because receivers close with their
    owner

### Still local (not Host APIs yet)

These still reach local code or the legacy Client directly. They stay in the
boundary baseline, assigned to later milestones. None of them is a Host API.

| Surface | Commands / code | Milestone |
|---|---|---|
| Projects, fleets, worktrees, project presentation | `list_projects`, `get_project`, `add_project`, `remove_project`, `reorder_projects`, `set_project_*`, `add_worktree`, `remove_worktree`, `init_repository`, `config.rs` | M4 |
| Git status and diffs | `git_changes`, `hidden_changes_projects`, `open_diff` | M4 |
| Files and previews | `list_dir`, `preview_file`, `read_image`, `open_file`, `open_path`, `reveal_file`, `inspect_folder`, `files_projects` | M5 |
| Ledger | `ledger_rows`, `ledger::watch` | M6 |
| Skipper and its folder, task types, watcher | `get_skiff_agent`, `set_skiff_agent`, `open_skiff_agent` (coordinator), `start_skiff_watch`, `skiff_watch_log`, `preview_skiff_folder`, `change_skiff_folder` (except its kill) | M6 |
| Agent settings and the resume list | `list_agent_sessions`, `set_agents`, `set_agent_command` (skiffd-served, legacy Client). `list_agents` moved behind `session_agents@1` in M3 | later |
| Endpoint-local lifecycle and diagnostics | `daemon_status` compatibility checks, `restart_daemon`, `reload_daemon`, `trace_set` | local only, token-guarded |

Client-owned surfaces (appearance, keys, menus, theme files, clipboard,
workspace files, traces and window state) stay in the client.

### Tests

- **Contract:** `m1g.json` round trips; exact reply kinds per request; defaults;
  an ignored coordinator field; bad roles, dimensions, layouts and directions
  refused.
- **Negotiated transport:** both families typed and separately gated; a service
  error stays typed; a wrong reply kind is malformed; nothing is sent without
  the capability.
- **Selected private socket** (`session_lifecycle.rs`): every operation compared
  with the legacy Client on one Client, covering exact frames, results, daemon
  errors, wrong-kind and unreadable replies, the lost-connection text, the
  unused second endpoint and a refused coordinator spec.
- **Fakes:** no capability, no source or another major means no call; 80
  concurrent kills complete without `host_busy` (at most 63 in flight);
  disconnect wakes a waiting call without replay.
- **Desktop entry point:** create, kill error, list and save group on the same
  connection as status, screen, inventory, updates and the terminal, plus the
  `host_capabilities` list.
- **TS** (`tests/hostCaps.test.ts`): offered call passes exact arguments; a
  missing or other-major capability is refused with no IPC; keystroke order
  holds across the first answer and later calls are synchronous; a reload
  re-reads the capabilities.

## M2 (first slice): remote terminals over SSH

### Topology: the verified adapter over a forwarded skiffd socket

Today's skiffd speaks its own newline-JSON protocol (PROTOCOL 27), not the Host
envelope. No Host-envelope server exists on any Host. M2 therefore does not
invent one. The same in-process adapter that M1 verified (`LegacyReadOnly`,
`LegacyTerminal`) runs over a different byte stream: the local end of an
OpenSSH stream-local forward to the remote skiffd socket.

- The app sends exactly the skiffd frames it sends locally. Parity is already
  proven frame by frame in M1b–M1g.
- The Host-envelope wire backend (`HostConnection::connect`) stays tested but
  unused. It becomes useful when a Host service speaks the envelope. That is
  the data-plane service of M4–M6, as decided in M1f.

### Transport and trust (`skiff-client::host::ssh`)

- **The system ssh:** `/usr/bin/ssh` (else `ssh` on PATH), so `~/.ssh/config`,
  ProxyJump, agents, hardware keys and known_hosts behave as in a terminal.
- **The forward:** `-N [-l <user>] [-p <port>] -L <private>/<pid>-<n>/skiffd.sock:<remote socket> -- <destination>`.
  `-l` and `-p` go only when the person set them; ssh config decides
  otherwise. Each forward makes its own `0700` directory, exclusively, inside a
  per-account, per-Host `0700` parent in the temp dir. Two windows or
  app runs on one destination never share, unlink or remove each other's
  socket. The path is short enough for `sun_path`.
- **Options forced on the command line**, so they win over ssh config:
  - `BatchMode=yes`, `StrictHostKeyChecking=yes`, `UpdateHostKeys=no`
  - `ExitOnForwardFailure=yes`, `ServerAliveInterval=15`/`CountMax=3`,
    `ConnectTimeout=15`
  - `StreamLocalBindMask=0177`, `StreamLocalBindUnlink=yes`
  - `ControlMaster=no`, `ControlPath=none`, `ForwardAgent=no`, `ForwardX11=no`,
    `PermitLocalCommand=no`, `RequestTTY=no`, `LogLevel=INFO` (so a refused
    channel can be explained)
- **Trust:** on the session link, an unknown or changed host key fails and
  nothing is learned or written. Skiff has no auth of its own and no TCP
  listener. The person trusts a key in Settings › Hosts, after a confirm
  (below), never on the link itself.
- **Destination validation:** one word, never starting with `-`. The socket
  must be an absolute path with no colon.
- **Failures** become plain causes (`SshError`): unknown key, changed key, no
  login without a prompt, unreachable, timed out, ssh exited, no skiffd at
  the socket, and forwarding refused.
  - OpenSSH 9.7 reports a missing remote socket and disabled
    `AllowStreamLocalForwarding` with the same line. Skiff reads it as "no
    skiffd" (`NoSkiffd`), then runs the probe (below): a live socket there
    makes it `ForwardingRefused`.
  - ssh's own output is never shown, because it can name key paths. No
    message names a command to run.
- **The server script** (`skiff-client/src/host/server.sh`): the one POSIX
  `sh` script Skiff runs on a server, sent on stdin with a mode. Its values
  (a socket name, a key line, a version, a hash) are checked against strict
  patterns and single-quoted.
- **The probe** (`skiff-client::host::check`): the script's read-only
  `probe` mode, over a strict, batch ssh run with the same trust options.
  It changes nothing and prints one JSON object: OS, arch, runtime folder,
  the socket and whether it is live, `skiff status --json` from the managed
  install, PATH or `~/.cargo/bin` when one answers, the managed install,
  the Skiff app's own skiffd (`/usr/bin/skiffd` from a .deb, or Skiff.app)
  if there is one, and how it keeps running. The socket name (from this
  build) and a socket the person set are its only arguments.
- **Host key trust** (the check list's Trust this key… and Review…): a
  short batch ssh run with `StrictHostKeyChecking=accept-new` into a private
  temp known_hosts only (`GlobalKnownHostsFile=/dev/null`,
  `PreferredAuthentications=none`) learns the key line, with ssh's own host
  name, port, alias and hashing. `ssh-keygen -l` gives its fingerprint, and
  the dialog shows it. Only on the person's confirm does the exact line go
  into the first `UserKnownHostsFile` that `ssh -G` names, once. A changed
  key shows both fingerprints and asks twice; the old lines go with
  `ssh-keygen -R`. The page never sends a key, only the token of one Rust
  scanned.
- **Key setup** (the Login row): `ssh -G` (identity files), `ssh-add -L`
  and `ssh-keygen -y -P ''` tell no key, a locked key and a usable one apart.
  - **Make key…** runs `ssh-keygen -t ed25519` for `~/.ssh/id_ed25519`, with
    no passphrase, only when the file is not there. Skiff never edits
    `~/.ssh/config`.
  - **Unlock key…** runs `ssh-add` with the passphrase (macOS:
    `--apple-use-keychain`).
  - **Install key…** runs one ssh with `BatchMode=no`,
    `StrictHostKeyChecking=yes`, `PreferredAuthentications=keyboard-interactive,password`,
    `NumberOfPasswordPrompts=1`, no agent, no master, no TTY (`setsid`). The
    script's `install-key` mode adds the public line only when that exact
    line is not there, and reports who owns `~/.ssh` and `authorized_keys`.
  - **Fix permissions…** (the key is in a file another account owns) runs
    the `fix-key` mode the same way: it renames `authorized_keys` to
    `authorized_keys.skiff-<time>` and writes a new one with this computer's
    key. Without ownership of `~/.ssh` there is no fix, only the cause.
  - **Copy public key** is the one copy action: for a server that allows
    keys only, the admin needs it.
  - **Secrets:** a password or passphrase goes from the dialog to Rust, then
    to ssh or ssh-add through an askpass helper (`SSH_ASKPASS_REQUIRE=force`,
    OpenSSH 8.4+) that reads it once from a FIFO in a private `0700` temp
    folder. It is never in a file, an argument, the environment or a log. A
    second prompt gets an empty answer, so ssh stops.
  - The public key line is the key modes' only value. It is checked against
    a strict pattern (known type, base64, plain comment) and single-quoted.
- **Server bundles** (#166 slice 4, for the install in slice 5): the release
  job builds `skiff-server-<version>-<os>-<arch>.tar.gz` for Linux x86_64 and
  aarch64 (static musl) and macOS aarch64 and x86_64
  (`scripts/server-bundle/build.sh`). Each holds `skiffd`, `skiff-host`,
  `skiff`, `manifest.json` (version, protocol, target, commit, the SHA-256 of
  each file) and, on Linux, the systemd user units. One job writes
  `SHA256SUMS` and signs each bundle and `SHA256SUMS` with minisign, with the
  file name as the trusted comment; it reads the key from the
  `SKIFF_SERVER_SIGNING_KEY` secret, and fails when that key is not the one
  the app carries. The app carries the public half
  (`skiff-client/src/host/server-bundle.pub`, key id 4B7D9FC893B9C075) and
  refuses a bundle whose signature fails or names another file.
- **Install and start on Linux** (#166 slice 5, `skiff-client::host::install`,
  `server.sh`), each after a confirm in Settings › Hosts:
  - **Install Skiff…** downloads the bundle for the server's arch from the
    release `v<app version>`, checks its signature with the app's key, and
    uploads it with one batch ssh run to
    `~/.local/share/skiff/server/.incoming-<sha256>.tar.gz`. A second run checks its
    SHA-256, unpacks it beside the old version, then moves the `current`
    link: a half-unpacked version never becomes current. It keeps the
    version before for a rollback, removes older ones, marks the install as
    Skiff's (`installed-by`), and links `~/.local/bin/skiff` only where that
    name is free or already ours. The same version again changes nothing.
  - **Start:** with a systemd user manager, the bundle's `skiffd.service`
    goes to `~/.config/systemd/user`, enabled and started, and lingering is
    turned on (`loginctl enable-linger`, the account's own right on most
    systems). Without one, `setsid nohup` starts skiffd and a pid file
    tracks it: it runs until the server restarts.
  - **skiff-host** (#166 slice 7) runs beside the managed skiffd: the
    bundle's `skiff-host.service` (`Restart=always`, no start limit), with
    the socket name of this build, enabled and started with skiffd; without
    a user manager, `setsid nohup` and `skiff-host.pid`. Start brings back
    whichever is down. An install that changed the version restarts it.
  - **Turn on…** asks for lingering later. When the server refuses it, the
    row says so in plain words; the Host works while the person is logged in.
  - **Never take over:** Install runs only where no skiffd answers and no
    managed install is there; Start runs only the managed one. A skiffd that
    Skiff did not install is used as it is, never replaced, stopped or started.
  - **Dev builds:** with no release for their version, a debug build sends
    its own `skiffd`, `skiff-host` and `skiff` (beside its executable), for a
    server with this computer's OS and arch only, unsigned. A release build
    never does.
  - No sudo, and nothing outside the account's home and runtime folder.
- **Install and start on macOS** (#166 slice 9): the same folder, upload and
  `server.sh` steps, with the macOS bundle. **Start** writes the LaunchAgent
  `~/Library/LaunchAgents/com.solutionscay.skiff.skiffd.plist` (`RunAtLoad`,
  restart after a crash only), and `…skiff-host.plist` for skiff-host (restart
  whenever it stops; an install of a new version restarts it with
  `kickstart -k`), and loads it with `launchctl bootstrap`: into
  `gui/<uid>` when this account owns the console, else into `user/<uid>`,
  which an ssh login reaches and which keeps running after logout (the rule
  `brew services` uses). An agent that is loaded already gets only a
  `launchctl kickstart`; a changed file waits for the next load, because a
  reload ends every session. No linger on macOS.
  - **A Mac with no login (headless):** a LaunchAgent does not start at boot
    before a login, and a LaunchDaemon needs admin, which Skiff does not use.
    So Skiff accepts "starts at login": after a restart, the Mac's own login
    starts it, and a Mac that nobody logs in to shows **Start** in the check
    list, which loads the agent again over ssh. Row 10 says so and passes.
- **Remove and uninstall** (#166 slice 8, `install::uninstall`,
  `server.sh`): **Remove…** forgets the Host on this computer, and its
  sessions keep running. When the install there is Skiff's, the confirm
  has a box: also stop and uninstall Skiff, which names the sessions that
  end. Checked, Skiff stops and disables its user services (on macOS: `launchctl
  bootout` of its LaunchAgents, and their plist files go), deletes
  `~/.local/share/skiff/server`, its unit files and the `~/.local/bin/skiff`
  link if it is Skiff's, then forgets the Host. A unit or a pid counts as
  Skiff's only when it runs from the managed folder. Projects, skiffd's
  session data and `local-host-id` stay. A failed uninstall keeps the Host.
- **Update with handover** (#166 slice 6, `install::update`, `server.sh
  update`). Up only: the app never moves a newer server down, and the
  script checks again under the lock. One install or update at a time: a
  `.lock` folder in the managed folder; a second one waits, then finds its
  version there. Steps: unpack beside the old version, move `current`, then
  `skiff daemon reload --binary <new>/skiffd` with the old version's CLI.
  skiffd execs the new binary with its handover, so the daemon pid, the
  session pids and the screens stay. A reload that fails before exec (the
  new skiffd cannot adopt the sessions) moves `current` back and names the
  live sessions; only the person's yes in the red dialog restarts skiffd,
  which ends them. With no live session it restarts without asking. A
  reload that did not come back moves `current` back and starts the old
  version. The version before stays as the rollback folder. After an
  update that moved the version, skiff-host restarts on it, with slice 7's
  restart (`systemctl --user restart`, the fallback's stop and start, or
  `launchctl kickstart -k` on macOS). Linux and macOS alike.
  - **On connect:** before the session forward opens, the app probes a Host
    once per app run. A managed install older than the app is updated with
    no prompt, but only with the handover; the status bar then says
    "updated from". The Host setting "Update Skiff on this server
    automatically" (`auto_update` in `hosts.toml`, on by default) turns it
    off. A Skiff that Skiff did not install is never updated.
  - **Update Skiff…** in the check list does the same after a confirm.
  - **Banners:** a newer server says to update this computer; a managed
    older one says "Update available" and opens its check list; a Skiff
    from its own install with an older protocol says that only the person
    there can update it.
- **Headless install** (#166 slice 6b): one command on a server with only a
  shell, `curl -fsSL https://github.com/solutionscay/skiff/releases/latest/download/install.sh | sh`,
  run as the account that owns the sessions, with no sudo. `install.sh` is
  `server.sh` itself: with no mode it runs the headless steps, and the app
  sends the same file with a mode. The release job
  (`scripts/server-bundle/install-sh.sh`) writes in the version, the
  SHA-256 of each bundle from `SHA256SUMS` and the public key. The script
  downloads the bundle for `uname -s -m`, refuses one whose hash differs,
  checks the minisign signature and its `file:<name>` comment when
  `minisign` is on the server (else it says it checked the hash only), then
  runs the same update and start steps as the app (`installed-by` is
  `install.sh`): skiffd and skiff-host, as systemd user units, the fallback,
  or LaunchAgents on macOS. Running it again updates with the handover; the same
  version changes nothing; it never moves down. When the handover is not
  possible, it names the sessions and goes on only with `--yes`. It ends by
  naming `user@hostname` (and one address from `hostname -I`) to add in
  Settings › Hosts. Each bundle carries `install.sh`, so
  `~/.local/share/skiff/server/current/install.sh --uninstall` runs the same
  uninstall as Remove… in the app: it stops what Skiff started (its units,
  LaunchAgents or pid files), removes the managed folder, the unit or agent
  files and our `skiff` link, and keeps `local-host-id`. With live sessions
  it asks on the terminal first, or needs `--yes`. It refuses a server
  where a Skiff from its own install holds the socket.
- **Remote commands:** the upload (one `sh -c` with the hash in its file
  name) and the server script's modes: probe, install-key, fix-key,
  install, update, start, linger, sessions and uninstall, and the same
  script run as install.sh on the server. The script is fixed in the app;
  its values (a version, a hash, a socket name, a key line) are checked
  against strict patterns and single-quoted.
- **The check list** (Settings › Hosts › Check, and Connect): reach the
  server, host key, log in without a prompt, server supported (Linux or
  macOS, x86_64 or ARM64), Skiff on the server (managed, from its own
  install, the Skiff app there while closed, or not installed), sessions
  service (skiffd), socket forwarding
  (a real `-L` forward), identity and version (a ping over it, compared with
  the pin, which it never stores), projects service (a Hello to
  `skiff-host` that must name the same Host id; a warning, with Start for a
  managed install) and keeps running after logout (both units enabled and
  linger; a warning: the Host can be used without it). A failed row skips the rows
  under it. It stores only the socket the probe found, unless the person set
  one.
- **Backoff:** after a failed open, the next waits 1, 2, 4 … up to 30 s.

### Identity (required remotely)

- A remote Host must report `identity` in its `pong` (M1f skiffd).
- With none, activation is refused ("reports no Host identity. Update Skiff on
  that Host."). The app's bootstrap id is never read or made for a remote Host.
- The forward's peer pid is this Mac's `ssh`. It is never shown or signalled:
  status reports no pid for a remote Host.
- **The pin.** The first accepted `host_id` is stored for that Host in
  `hosts.toml` (`host_id`) before the connection is used, compare-and-set
  against the file. Every later connection, through a reconnect, a new
  selection or a new app run, must report the same id. A different one is
  `identity_changed`: Skiff connects to nothing there, and the status shows an
  `identity` warning.
- **Only the person changes it.** The warning's "Accept new identity…" asks
  first, then `accept_host_identity` stores the id Rust saw and refused (never
  one the page names) and reloads the page. Forgetting the Host and adding it
  again also starts over.

### Lifecycle

- `ProcessControl` exists only for a local endpoint, so a remote Host never
  gets:
  - daemon spawn
  - stop/replace
  - restart
  - reload plan and run
  - CLI link
  - watcher start/replace
- **The remote connect path skips the reload plan entirely.** A protocol
  mismatch is reported as a `remote` warning ("Update Skiff on that Host"),
  with no Restart button.
- **The app's own transport:** stopping or dropping the forward (switching
  Host, quitting) ends only the `ssh` process and removes its private
  directory. Remote sessions run on, and the tests prove it.

### Input, backpressure and reconnect

- **Input is never held for a remote Host.** While the link is down, `pty_write`
  refuses it ("Input is off …"). The page also mutes the terminals and shows
  the Host label as disconnected.
- **A keystroke to a connection that is already closed** is refused by
  `Client::input_now`. Before this change the keystroke vanished into a dead
  queue; that bug was found in the SSH test and fixed for local too.
- **Backpressure:** a remote connection bounds queued input at 1 MiB
  (`REMOTE_INPUT_LIMIT`). More is refused as `host_busy` with a message, and
  nothing is kept to send later. Local input stays unbounded, as before.
- **Link loss detection:** the event task checks the connection each second and
  emits `skiff:host-lost`.
- **The page then reconnects:** `daemon_status` with 1→30 s backoff (each try
  is ssh, handshake and identity check), then capabilities, events, the
  session list, groups if offered, and every shown pane re-attached for a
  fresh snapshot.
  - Any failed step fails the whole try. A loss reported during a try is kept.
    Either way the whole recovery runs again, after 1→30 s.
  - Input comes back on, and the Host shows connected, only after a complete
    try with no loss in it.
- **Why it is down:** each failed connect leaves its cause in the status
  warning (`remote`, or `identity`): an `SshError` text, a Host without
  identity, no answer, or one plain line with what to check. ssh's output and
  local paths are never shown. A backoff wait keeps the last cause.
  - The real-SSH test shows SameInstance continuity after the link returns.
  - Nothing typed while down arrives later.
- **No retries:** mutations are not retried; lifecycle is not offered remotely
  in M2 anyway.

### What a remote Host shows

- **Capabilities:** a remote Host gets status, screen, inventory, updates and
  terminal. Since M3 it also gets lifecycle, groups and agents when its
  skiffd speaks this app's protocol (see M3).
- **Local data:** projects, Git, files and local openers, the ledger, Skipper,
  agents, `projects.toml` and daemon lifecycle are refused in Rust by one gate
  (`LOCAL_ONLY` in `lib.rs`, checked against the selection). A test fails if a
  new command is not placed on one side. The page also does not load them:
  - no projects
  - the ledger does not start
- **Host-owned workers on this Mac:** the Skipper skill copy
  (`skiff_agent::sync`) runs only while this Mac is shown, once per app run:
  at launch, or on the first switch back to This Mac. The ledger watcher reads
  nothing while a remote Host is shown, and starts fresh after a switch back.
  - the welcome screen says "Sessions on <Host>"
  - the sidebar lists sessions under Other
- **OSC 52:** remote clipboard writes are ignored unless allowed for that Host
  ("Allow clipboard writes from this Host…"). The permission is stored as the
  pinned Host id it was given to (`clipboard_host`) and counts only while that
  is the pin: a Host never connected cannot get it, and an accepted new
  identity starts blocked. Reads stay unsupported, as everywhere.

### Picker and label

- **The Host cell in the top bar** shows a state dot and the name ("This Mac"
  or the Host's shown name). The status bar's skiffd label and the window title
  (`Skiff — <Host>`) name it too.
- **Its menu** picks a Host, opens Settings › Hosts at its Add form (Add SSH
  Host…) or its list (Manage Hosts…), and toggles the clipboard opt-in.
- **Settings › Hosts** lists the known Hosts and adds, edits, renames and
  removes them. The Server field lists the aliases in `~/.ssh/config`. The line
  under it shows what `ssh -G` resolves (user, hostname, port, jump host),
  with no network.
- **A Host that needs attention** (its last check or connection in this app
  run failed, or no socket is found yet) shows "Needs attention" in the menu.
  Picking it opens its check list instead.
- **Picking a Host** tears down the old connection, streams and held input,
  then reloads the page: no view mixes two Hosts.
- **`hosts.toml`** (client-owned, beside `projects.toml`) stores the known
  Hosts, each with a local `id` that never changes, a shown `name`, the
  `destination`, an optional `user` and `port`, the `host_id` pin and
  `clipboard_host`. `selected` names an `id`. No socket is stored: each Host
  runs one skiffd, on `<runtime dir>/skiff/skiffd.sock` there, and the probe
  that runs on each connect (and in the check list) finds it; the app keeps
  it for the run. No two Hosts share a destination, user and port. An older
  `clipboard = true` is ignored. An unknown or invalid selected entry falls
  back to This Mac.

### Agent list: M3

The agent list moved with M3 (below). Agent settings and the resume list stay
local-only.

### Demo

1. **On the remote Host:** nothing, on Linux: Install Skiff… in the check list
   installs and starts it. Its sshd must allow stream-local forwarding;
   OpenSSH allows it by default.
2. **On this Mac:** `ssh <alias>` must work without a prompt (key in the agent
   or ssh config), and the host key must already be in known_hosts.
3. **Start this V2 build.** Use a private local state, so it does not touch a
   v1 daemon.
4. Click the Host cell, then Add SSH Host… Pick the alias, then Connect. The
   check list runs and finds the socket. Switch to the Host from there.
5. Remote sessions list under Other. Open one, type, paste and resize. On the
   remote Host, start more with `skiff new --agent <id>`.
6. **Drop the link** (Wi-Fi off or laptop sleep). The cell turns red, input is
   off, and the status bar says why. Restore the link: Skiff reconnects and
   redraws each pane from a snapshot.
7. **Quit the app.** The remote sessions keep running; check with `skiff ls`
   there.

### Tests

- **Unit** (`host::ssh`): option set and order (strict trust, nothing accepting
  new keys, `--` before the destination), refused destinations and sockets,
  stderr→cause mapping without echo, private-directory refusal.
- **Real SSH** (`crates/skiffd/tests/ssh_remote.rs`): a private skiffd, a
  loopback sshd with throwaway keys and known_hosts, the system ssh with
  `-F <fixture config>`. Nothing touches `~/.ssh`.
  - Attach, input and output over the forward; the remote adapter's
    capabilities; no `ProcessControl`.
  - Link drop: closed stream, input refused, session alive.
  - Down-link error, then backoff.
  - Reconnect gives SameInstance, a fresh snapshot, and no "LOST" input.
  - Quit removes the forward's own directory; the session survives.
  - Unknown key, changed key (known_hosts unchanged), wrong socket, forwarding
    disabled, and a remote skiffd without identity refused, with no bootstrap
    file.
  - It skips when this machine has no usable unprivileged sshd.
- **Desktop:**
  - a remote endpoint runs neither the local daemon binary nor a second ssh
    (backoff), and status shows `destination:socket` with no pid
  - every command is classified local-only or allowed
  - remote input is off while disconnected
  - `hosts.toml` round trip; an unknown or invalid selection is This Mac
  - the pin: accept A, refuse B from the file, accept B only explicitly; the
    clipboard permission follows the identity
  - an unknown host key's cause reaches the status from a fresh state and
    stays during the backoff; every failure maps to a cause without paths
  - a remote launch does not copy the Skipper skill (private config); the
    ledger watcher reads nothing while remote
- **Fake ssh** (`skiff-client/tests/ssh_forward.rs`, the M2 review's
  fixtures): a changed identity stays refused after reselection; two forwards
  to one destination keep separate sockets.
- **Page:** `hosts.test.ts` (clipboard opt-in per Host, pick saves then
  reloads, one recovery per lost link, and the review's race: a second loss
  during recovery runs it again and input stays off until it is clean) plus the
  existing suites.
- **CI** now runs `tsc` and the page tests (`desktop-ts` job) and
  `ssh_remote`.

### Not yet (M2 acceptance gaps)

- No manual run on two machines yet: the Termius side-by-side, the Wi-Fi drop
  and the laptop sleep in the M2 "done when". CI has not run (not pushed).
- Remote session create, rename, end and groups, the agent list and resize
  contention: done in M3 (below).
- The picker has no "test connection" step. Errors show when the Host is picked.
- Remote URL activation is unchanged (it opens only after a click, as before).
  The security review before M2 ships to anyone is still open.
- Latency was not measured over a real link (M1f topology follow-up).

## M3: remote session lifecycle (#159)

### What a remote Host now does

- **Lifecycle and groups** go through the same M1g `session_lifecycle@1` and
  `session_groups@1` paths and the same legacy skiffd frames as on This Mac:
  `create_session`, `rename_session`, `kill`, `set_session_theme`, `seen`,
  `list_groups`, `save_group`, `delete_group`. Nothing new runs on the Host.
- **Create in a folder on the Host.** `SessionSpec.cwd` is a path on the Host,
  sent as typed; `null` is the Host account's home, chosen there. The app
  never resolves, expands or checks it against this Mac. "New session…" on a
  remote Host (its key, the menu bar, the palette, the welcome screen) opens a
  folder menu:
  - the folders the Host reported, from its sessions and groups, with the
    highlighted or focused session's folder first
  - Home folder
  - Type a folder…: an absolute path on the Host, or empty for its home. `~`
    and relative paths are refused, because skiffd does not expand them.

  Each folder opens Shell, then the Host's agents. Split and the group start
  menus already used the session's or group's own folder.
- **Rename, theme and seen** act on the remote session.
- **End** asks first, as on This Mac. End, Kill all, Remove and Close keep
  their confirmations. A shell that exits is removed from the list without a
  question, as before; it has no process left.

### The agent list: `session_agents@1`

- A narrow, read-only family:
  - `agent_list` answers `agents { agents: [HostAgent] }`
  - `HostAgent` is `{ id, command, default_command, custom, installed, enabled }`,
    a lossless mapping of skiffd's `AgentInfo`
  - golden fixture: `tests/fixtures/m3.json`
- The local adapter sends skiffd's existing `list_agents` frame. skiffd
  answers from **its own account**: its PATH (the login shell's), and its
  `projects.toml` `[agents]`. For a remote Host that is the remote account,
  never this Mac's config.
- The `list_agents` Tauri command now goes through the selected Host and is no
  longer local-only. The page loads `S.agents` with
  `hostInvoke("session_agents", "list_agents")`, so a remote Host's start
  menus offer its own agents and its own command lines.
- **Still local-only:** `list_agent_sessions` (the resume list runs a command
  that the page names), `set_agents`, `set_agent_command`, `rename_agent`,
  `remove_agent` and `restore_agents`. On a remote Host, Settings › Agents
  lists that Host's agents read-only and says to change them on that Host.

### Capabilities say what the endpoint can do

- **This Mac:** every family, as before. The app reloads or replaces a skiffd
  that speaks another protocol.
- **A remote Host:** status, screen, inventory, updates and terminal always.
  `session_lifecycle`, `session_groups` and `session_agents` only when its
  `pong` reports this app's `PROTOCOL`. The app never changes a remote skiffd,
  and these mutations are the legacy frames that `PROTOCOL` versions. On
  another protocol they are not advertised, the page hides their actions, and
  a direct call is refused before any frame. The welcome screen says why.
- **Peek tools are refused remotely**, though the local adapter starts them.
  A peek shows a file of this Mac. A remote `session_create` with
  `peek: true` is a `permission` error with no frame.

### No replay

- Each mutation is written once. A lost or timed-out reply is an error
  (`transport_lost` or `timed_out`), and the mutation may have run.
- Nothing retries it: not the adapter, not the reconnect, not the page's
  recovery loop (that loop re-reads state; it sends no mutation).
- A later request is the person's new action, and it goes once.

### Resize contention (several attached clients)

This is the existing policy. It is now written down and tested over a real
forward:

- A session has **one PTY size**. skiffd applies resizes in the order it
  receives them, from any client. **The last one applied is the size** for
  every attached client.
- skiffd sends every connection a `session_updated` event with each new size.
- **Nobody re-asserts.**
  - The app does not answer another client's size with its own. `ptySized`
    forgets the size it sent, and the app sends a size again only when one
    of its own panes changes size (a fit after a drag, show or zoom).
  - `skiff attach` (Termius) sizes the pane on attach and on SIGWINCH. On
    detach it gives the pane back its earlier size, unless another client
    sized it meanwhile.
- Two resizes at the same time: whichever skiffd applies last wins, and every
  client sees that one. No client loops.

### Guards kept

- **Identity and trust:** M2's pin, `identity_changed` and strict ssh options
  are unchanged. A mutation needs an accepted connection like any request.
- **Local lifecycle tokens:** unchanged. A remote Host never gets
  `ProcessControl`.
- **Remote paths never reach a local opener.** Session and group folders on a
  remote Host go only into session specs and group saves. `open_path`,
  `open_file`, `reveal_file`, `preview_file`, `read_image`, `list_dir`,
  `open_diff` and `git_changes` stay in `LOCAL_ONLY`. Rust refuses them while
  a remote Host is shown, and a test keeps them listed. Every menu item that
  names a folder to an opener uses a local project or the Skipper folder,
  which a remote Host does not load.

### Tests

- **Contract** (`skiff-host-api/tests/contract.rs`): `m3.json` round trip; the
  agent list's own capability and one reply kind; refused malformed rows.
- **Selected local socket** (`session_lifecycle.rs`): the agent list sends the
  legacy `list_agents` frame; same result and error text as the legacy Client.
- **Fake ssh** (`ssh_forward.rs`):
  - a compatible remote Host serves create (Host cwd as typed, and `null` for
    home), rename, kill, groups and its own agents, over the exact frames
  - a remote Host on PROTOCOL−1 or +1 gets none of the three families; each
    call is refused with no frame
  - a remote peek create is refused with no frame
  - a create whose reply is lost is written once and never resent after the
    reconnect
- **Real SSH** (`ssh_remote.rs`, private skiffd and loopback sshd):
  - the agent list is the Host config's own
  - a session starts in a Host folder and runs there (`pwd`)
  - rename, group save (the Host's CLI sees it), list and delete, and end
  - the last-applied resize is the size for the app over the forward and a
    Host-side client; the app hears the foreign size; nothing re-asserts
- **Desktop:** the capability list includes `session_agents@1`;
  `list_agents` is a Host command, and the agent settings and the resume
  list stay local-only.
- **Page:** `remoteLaunch.test.ts` (Host folders once each, the asked folder
  first; typed folders absolute or empty; the Host's agent starts in the Host
  folder; Cancel starts nothing) and `hostCaps.test.ts` (`session_agents`
  gating, no call without it).

### Not yet (M3 acceptance gaps)

- No manual run on two machines. CI has not run, because nothing is pushed.
  M2's own manual and CI acceptance is still open on #158.
- The resume list and agent settings on a remote Host.
- A folder browser on the Host: folders are the reported ones, home, or
  typed. Browsing needs the M5 files API.
- A remote skiffd on another protocol gets no lifecycle. That Host's own
  `skiff` CLI is the way to start work there.

## M4 (first slice): the data-plane service, read-only projects and Git status

Tracking: [M4 #160](https://github.com/solutionscay/skiff/issues/160), [epic #155](https://github.com/solutionscay/skiff/issues/155).

### What runs where

- **`skiff-host`** (`crates/skiff-host`) is the Host's data-plane service. It
  is a separate process from skiffd, as #62 and the M1f topology require:
  - It listens on its own socket beside skiffd's: `x.sock` gives
    `x-host.sock` (`skiff_core::socket::data_socket`), with the same `0700`
    directory and `0600` socket rules as skiffd.
  - **Its path is never taken over.** An entry already there is checked
    without following links (`skiff_host::bind`):
    - a live socket means another service, and this one does not start
    - a stale socket of this account is removed once it is the same inode
      as the one checked
    - anything else is refused and kept: a regular file, a symlink, a folder
      or another account's socket
    - on exit it removes its socket only if the path still names the inode
      it bound, never a newer service's socket
  - `--session-socket <skiffd socket>` names the skiffd it pairs with. Its
    state follows that socket exactly as skiffd's does: a private test
    socket (`SKIFF_SOCKET`) gets its own `projects.toml` and Host id. Every
    build, debug or release, pairs with the one `skiffd.sock` otherwise.
  - It holds no PTY, no session and no client of skiffd. Stopping or
    restarting it never touches a terminal.
- **Direct topology.** The app reaches it directly, never through skiffd and
  never as a proxy in front of skiffd:
  - This Mac: the socket beside skiffd's.
  - SSH Host: a second forward, to the socket beside the remote skiffd's
    socket. The path is computed by path arithmetic only, never resolved on
    this Mac.
- **Source ownership.** The service runs the code the app ran before:
  `skiff_core::project::list`, `Project::from_config`, `git::list_worktrees`
  and `git::changes`. Nothing is copied. The desktop commands
  `list_projects`, `get_project` and `git_changes` no longer call `skiff_core`.
  They ask the selected Host's data plane.

### Contract (additive, API major 1)

- `projects@1`:
  - `project_list` answers `projects { projects: [HostProject] }`, open and
    closed, in config order.
  - `project_get { name }` answers `project { project }`, or `not_found`.
  - `HostProject` and `HostWorktree` have the same JSON as the app's
    `Project` and `Worktree`. Every path is a Host path. `icon` is a data URL
    (bytes). `background` is a Host path that only that Host reads; M5 serves
    its bytes.
- `git_status@1`:
  - `git_changes { worktree }` answers `changes { changes: [HostChange], base? }`.
    `status` is one of `M`, `A`, `D` or `U`. On a branch, the files count
    from the fork point with the project's default branch, committed or not,
    and `base` names that branch; `committed` is true for a file the working
    tree has as HEAD does. On the default branch, `base` is absent and the
    rows are the uncommitted files, as before. A Host without these fields
    reads as `base` absent and `committed` false.
  - `git_diff { worktree, file? }` compares against the same base.
  - `worktree` must be a worktree of one of the Host's open projects. Any
    other path is `not_found`, and git does not run there.
- Golden fixture: `tests/fixtures/m4.json`. Neither family grants a session
  operation, and neither grants the other.
- **Hello:** the service's own `instance_id`, a new one per start.
  `service_versions` names `skiff-host`. `limits` are 8 MiB frames and 16
  requests in flight per connection. An unreadable or oversize frame ends
  that connection only.
- **Admission is bounded through the write.** A request holds its slot from
  its frame until its reply is written. At 16 the service reads no further
  frames, so a client that stops reading stalls there: nothing more is
  computed, encoded or queued for it. The requests belong to the
  connection: when the client goes, they are dropped.
  `skiff_host::admitted()` counts them, for diagnostics and tests.
- **Line counts never leave the worktree** (`skiff_core::git::changes`, the
  same for v1 and the service). Untracked files are opened one component at
  a time below the worktree, with `O_NOFOLLOW`:
  - a symlink counts as one line, its target, as git counts it; it is never
    followed
  - a path through a symlinked folder, or `..`: no count
  - a FIFO, device, socket or folder is checked by type first and never
    opened: no count
  - a file over 1 MiB, or one that grows past it while it is read: no count,
    and no more than the limit is read

### Identity and continuity

- `host_id` comes from the same store skiffd reads (`local-host-id` beside the
  selected `projects.toml`). A service that cannot read it does not start.
- `HostSelection::connect_data(host_id)` connects only to a service whose
  `host_id` is the one the session plane accepted:
  - for a remote Host, that is the pinned one
  - any other is `identity_changed`, and nothing is sent to it
- **Continuity** is tracked per selection, apart from skiffd's. A restart is
  `NewInstance` on the data plane and changes nothing on the session plane.
- **Nothing is replayed.** A lost data connection fails its requests with
  `transport_lost`. The next read connects again.
- **Remote waits.** Remote tries wait 1 → 30 s after a failure. A remote
  Host with no service is `NotRunning`.

### The app

- **Capabilities.** `host_capabilities` returns the session plane's list,
  plus the data plane's when it answers. This is still one list for one Host.
  The page gates on `projects` and `git_status`.
- **This Mac.** `data_now` starts `skiff-host --session-socket <selected
  socket>` when none answers:
  - It needs the `ProcessControl` token, as the skiffd spawn does.
  - The binary is `SKIFF_HOST_BIN`, else the one beside the app.
  - The dev build builds it (`beforeDevCommand`), and the bundle ships it
    (`sidecar.sh`, `externalBin`).
  - If it does not start, the projects list shows why. No local read stands
    in.
- **A remote Host.** The app starts `skiff-host` there only for a managed
  install, from Settings › Hosts (#166 slice 7). Without a service, the page shows that Host's sessions only, as in
  M2/M3. With a service, the sidebar shows that Host's projects, fleets,
  worktrees and change counts. They are read-only:
  - Project, worktree and fleet menus offer New session and Copy path only.
  - Drag-to-reorder, project keys, project themes and backgrounds do nothing.
  - Change rows show no Open, and copy the Host path only. Since the second
    slice they show the Host's diff text in the page's own viewer.
  - Rust still refuses every write and every opener while a remote Host is
    shown (`LOCAL_ONLY`).
- `list_projects`, `get_project` and `git_changes` left `LOCAL_ONLY`. The
  command classification test keeps them on the Host side.

### Demo

1. **This Mac.** Run the dev app. The projects list loads as before. In the
   process list, `skiff-host --session-socket …/skiff/skiffd.sock` runs beside
   skiffd, one per computer; an older one is replaced by the newer app's.
   - Stop that `skiff-host`. Terminals keep working. The next project read
     starts it again.
2. **An SSH Host.** Install Skiff… in Settings › Hosts starts `skiff-host`
   next to its skiffd. On a Host with its own install, start it there with
   the same `SKIFF_*` environment as that skiffd, or none for the default.
   - Pick the Host in the app. Its projects, fleets, worktrees and changed
     files show in the sidebar, under that Host's paths.
   - Stop it there. Terminals keep working.
   - Without it, the page shows sessions only.

### Tests

- **Contract** (`contract.rs`): the `m4.json` round trip, one reply kind per
  request, and refused malformed rows.
- **Service** (`skiff-host/tests/data_plane.rs`, in process, private state):
  - Parity: the JSON of `project_list` and `git_changes` equals
    `skiff_core::project::list` and `git::changes`. The fixture has a fleet,
    a second worktree, a closed project and a missing one.
  - Git is refused outside known worktrees: a closed project's repo, `/`,
    a missing path and a relative path.
  - Session families are unsupported. Another Host's id is refused.
  - A restart: `transport_lost`, then `NotRunning`, then `NewInstance` with
    the same Host. A bad frame ends one connection only.
  - An allowed worktree's untracked link to a FIFO outside it answers at
    once, counted as one line.
- **Review fixes** (`skiff-host/tests/socket.rs`, `bounds.rs`, and the core
  test `untracked_links_fifos_and_large_files_are_never_followed_or_read`):
  - **Socket:** a regular file, a symlink and a folder at the path are
    refused and kept, and a live socket too. A stale socket is replaced.
    Release keeps a newer owner's socket. The binary refuses a regular file
    and keeps its bytes.
  - **Admission:** a client that never reads holds the service at exactly 16
    admitted. On disconnect it drops to 0 and the connection ends. A client
    that reads gets all 200 replies.
  - **Counts:** links to an outside file, FIFO and folder count 1, matching
    git's numstat. A 1 MiB+ file has no count. An in-repo FIFO, a path through
    a link, `..` and absolute paths are refused.
  - SSH (a fake `ssh` that pipes to a socket on this machine): the forward
    names the socket beside the remote skiffd's. No service gives
    `NotRunning`, then a wait, and nothing is started.
- **Two real processes** (`skiff-host/tests/planes.rs`): a private skiffd and
  `skiff-host`.
  - One Host on both planes: id, os and account, with two instances.
  - A PTY echoes before, during and after a SIGTERM and restart of
    `skiff-host`, with the same pid and an open session connection.
  - The restart is `NewInstance`. skiffd's instance does not change.
- **Real SSH** (`ssh_remote.rs`, loopback sshd): a real `skiff-host` on the
  Host side serves that Host's project and changes over a second forward,
  with the same Host on both planes. When it stops, its requests are lost,
  the next is `NotRunning`, and the remote terminal still echoes.
- **Desktop:** `data_now` starts this Mac's service once with
  `--session-socket <selected socket>`, reuses it, and refuses one with
  another Host id. A remote selection runs no `skiff-host`. The command
  classification is updated.
- **Page** (`projects.test.ts`):
  - A remote Host with `projects` reads only its own list.
  - A remote Host without it reads nothing.
  - This Mac reads through the data plane with its settings. Its failure
    shows the cause, and no local data stands in.

### Not yet: requirements before M4 is complete

- **Writes.** Done in the third slice (below), except image paths.
- **Config migration** (§5 of #155). Done in the third slice, as a copy.
- **Diff text.** Done in the second slice (below).
- **Per-project display settings.** `changes` comes from the Host since the
  second slice. `files_projects` and `project_themes` still come from this
  Mac's config, and are not loaded for a remote Host.
- **Supervision.** Closed by #166 slice 7 for a managed install on Linux:
  Install Skiff… starts `skiff-host` beside skiffd as a systemd user unit
  (`Restart=always`), or without a user manager in the background until the
  server restarts. The check list's projects row offers Start when it is
  down, and an install of a new version restarts it. A Skiff the person
  installed is still theirs to run. `skiff status` does not show it.
- **Change watching.** The page still reads again on session events. The
  Host has no data-plane event stream yet.
- **Stale capabilities.** Fixed in the second slice: the page follows the
  data plane and reads again when it answers.
- **Acceptance.** No manual run on two machines yet, and CI has not run
  (nothing is pushed). M2 and M3 manual and CI acceptance also stay open.

## M4 (second slice): diff text, data-plane recovery, Host display metadata

Tracking: [M4 #160](https://github.com/solutionscay/skiff/issues/160). Read-only,
like the first slice. Writes and config migration are the next M4 increment.

### Contract (additive, API major 1)

- **`git_diff@1`**, served by `skiff-host` beside `git_status@1`. It is a
  capability of its own: `git_status` still grants no diff text.
  - `git_diff { worktree, file? }` answers `diff { diff: HostDiff }`: one
    changed file's diff against HEAD, or, without `file`, the whole
    worktree's. A request without `file` is the whole worktree's.
  - `HostDiff`: `text`, `truncated`, `binary`, `untracked`.
  - `worktree` is checked as for `git_changes`. `file` must be relative,
    with no `..`, `.` or root (`skiff_core::git::is_relative_file`), else
    `not_found`.
- **`HostProject.changes`** (and `skiff_core::project::Project.changes`):
  the Host's `changes = false` setting, so the Changes section follows that
  Host's `projects.toml`. Absent means shown. This is the only display key
  added; `files` and themes stay later work.
- Golden fixture: `m4.json` has the capability, both request forms, a text,
  a binary untracked and an empty reply, and the new project field.

### How the Host makes the text (`skiff_core::git::diff_in`)

- **The held root.** The same rules as the counts: the service opens the
  named folder before `known_worktree`, and the path must name that folder
  (device and inode) before git runs and after. A swap is refused.
- **Tracked files and the whole worktree:** `git diff --no-color
  --no-ext-diff --no-textconv --no-renames <HEAD> [-- :(literal)<file>]`
  with `core.quotePath=false` and `GIT_OPTIONAL_LOCKS=0`. The Host's
  external diff tool and textconv filters never run. The pathspec is
  literal, so `*` in a name is not a pattern. A repository with no commit is
  compared with the empty tree.
- **Untracked files** are not given to git. They are read under the held
  root with the count's rules (`O_NOFOLLOW` per component, a type check
  before any open, non-blocking) and drawn in the `git diff --no-index`
  form. A link shows its target and is never followed. A FIFO, device or
  folder is refused with a message. git lists no FIFO as untracked, so a
  request for one answers "no change".
- **Bounds:** at most 512 KiB of text (`DIFF_LIMIT`). Longer text is cut at
  a line end and marked `truncated`. git's output is read with a limit; past
  it, or past 20 s, git is killed. Its stderr is drained and only its start
  is kept. Text is UTF-8 with invalid bytes replaced, so a reply stays
  within one 8 MiB frame.
- **Binary:** git's note (`Binary files … differ`) with `binary`. An
  untracked file is binary when it has a NUL in its first 8000 bytes, as
  git decides.

### The app

- **Remote diffs in the page.** On a remote Host that offers `git_diff`,
  a change row and the Changes row show the Host's text in the peek, in a
  viewer of its own (`terminal/diffView.ts`). No command runs on this Mac.
  The Host's paths are never read or opened here (`open_diff` stays
  `LOCAL_ONLY`).
  - The viewer says what it shows: loading, no change, untracked, binary,
    cut at 512 KiB, the whole worktree without untracked files, or the
    cause of a failed read.
  - Esc closes it. Space opens or closes the row. The arrow and page keys
    scroll.
  - This Mac keeps its `[open] diff` command in the peek, unchanged.
- **Recovery** (`app/dataPlane.ts`, `data_plane_status`). The page checks
  the data plane on its own: every 5 s while it answers, and 1 s, doubling
  to 30 s, while it does not. The check uses the live session plane only: it
  never connects skiffd or touches a terminal stream. On This Mac a missing
  `skiff-host` starts, as for any data read.
  - A new data connection (the service started late, restarted, or the link
    came back) reads the capabilities, projects, Git changes and an open
    remote diff again. No page reload.
  - A lost one shows its cause in the list. The last list stays, and says
    so.
- **No stale replies.** Rust uses a data reply only if its connection is
  still the selected Host's (`data_read`). Each new connection, or its
  loss, is a new data epoch in the page. A Git list or diff asked in an
  older epoch is dropped and asked again. A Host switch reloads the page,
  as before.
- **States.** The list says `Loading projects…`, the cause when the data
  plane is out of reach, and `No projects` for an empty list. The old "show
  in a later version" note is gone.

### Demo

1. **An SSH Host** with `skiff-host` running there: pick it, open a
   worktree's Changes, and select a changed file. The viewer shows the
   Host's diff. Select an untracked file, a binary file and the Changes row
   itself (Enter, or Review all changes).
2. Stop `skiff-host` there. Within seconds the list says why, and the
   terminals keep working. Start it again: the projects and changes come
   back without a reload.
3. **This Mac:** a change row still runs `[open] diff` in the peek.

### Tests

- **Core** (`git.rs`): `diff_text_is_bounded_plain_and_stays_in_the_worktree`
  (parity with git's own text, no color, an unborn repo, a literal
  pathspec, no change, untracked text, a link not followed, a FIFO that
  never blocks, binary tracked and untracked, cut tracked, untracked and
  whole diffs, refused paths, a refused root swap) and
  `a_diff_process_is_bounded_in_time_and_output` (killed after its time,
  killed past its limit, a failure's stderr).
- **Contract:** the `git_diff` capability, requests and replies in
  `m4.json`; one reply kind; a request without `file`.
- **Service:** `data_plane.rs` diff parity with `diff_in`, untracked, whole
  worktree, a cut diff in one frame, and refused paths and projects.
  `root_swap.rs`: a controlled `git` swaps the root after `status`, and the
  untracked diff is refused, not read.
- **Real SSH** (`ssh_remote.rs`): the diff of the Host's change over the
  data forward.
- **Desktop:** the data plane status on a new connection and after a
  restart of the service (no second start), and a reply on a replaced
  connection is refused (`DATA_REPLACED`).
- **Page:** `dataPlane.test.ts` (late start, restart, link back, waiting,
  epochs), `diffView.test.ts` (line kinds, text never parsed as markup,
  every state), `changes.test.ts` (which diff a row shows, the Host's
  `changes`, a stale Git list dropped and asked again), and
  `projects.test.ts` (load states).

### Not yet (after the second slice)

- Writes and config migration, as listed for the first slice.
- `files` and project themes from the Host, and a remote file tree (M5).
- A running git that hangs after its output closes is still waited for by
  its `spawn_blocking` worker; the 20 s limit kills git itself, not a
  process that git started and that holds its pipe.
- Data-plane events: changes are read again on session events and on the
  check, not pushed.
- Manual two-machine and CI acceptance.

## M4 (third slice): project and worktree writes, client settings split

`skiff-host` is now the only writer of the Host's `[[project]]` tables and
project worktrees. The app sends each change to the selected Host, This Mac
and an SSH Host alike, and runs no `skiff_core::config` project edit and no
worktree git of its own.

### Contract (additive, API major 1)

- **`project_config@1`**: `project_inspect` (answers `folder`, a
  `HostFolder` whose icon is bytes), `project_add` (answers `project_added`
  with the row and the new revision), `project_edit` (one key: `short`,
  `fleet`, `color`, `theme`, `changes`, `files`, `background_opacity`,
  `closed`), `project_arrange` (order and fleets in one write) and
  `project_remove`. Each of the last four answers `revision`.
- **`worktrees@1`**: `worktree_add` (answers `worktree`) and
  `worktree_remove` (answers `ok`).
- `projects` (the `project_list` reply) gains an optional `revision`. Rows
  gain `theme` and `files`, which are left out when unset, so the M4 golden
  rows are unchanged.
- A new error code, **`invalid`**: a value the Host does not take.
- Neither family is granted by `projects@1`, `git_status@1` or each other.
  Golden shapes: `tests/fixtures/m4b.json`.

### Validation, conflict and no replay

- **Revision.** A token over the parsed `[[project]]` tables only (FNV-1a
  over their JSON). Keys, appearance and agents do not count. Every project
  change names the revision it was made from. The Host checks it under the
  config edit lock, before any write. A changed revision is `conflict`, and
  nothing is written. The same lock covers the edit itself, so of several
  writers that start from one read, exactly one is written.
- **No replay.** The client sends a change once. A lost connection or a late
  reply is reported as "may or may not have been made", the page reads
  again, and nothing is sent again. A stale or repeated project change is
  refused by its revision. A second `worktree_add` is refused by git, and a
  second `worktree_remove` is `not_found`. The app keeps the last revision
  per Host id. A conflict forgets it, so the next change is not sent until
  the page reads the projects again.
- **Validation** (typed): unknown project or worktree is `not_found`; a bad
  color, initials longer than 3, a label over 64 characters or with control
  characters, a relative or multi-line folder path, a branch name git would
  not take or that would leave `<dir>-worktrees`, and the main worktree are
  `invalid`; a name or folder already added, a live session in the worktree
  and uncommitted changes without `force` are `conflict`.
- **Worktree removal safeguards stay on the Host.** `skiff-host` reads its
  own skiffd's sessions (`list_sessions` on the paired socket) before it
  removes anything. If it cannot read them, the worktree stays. The client's
  word is never used. `force` is a second request the person confirms, never
  a retry.
- **Images.** `icon` and `background` are paths on the Host, and no
  capability carries image bytes yet. Their menu entries stay on This Mac
  only, as local commands. A remote Host's project shows no such entry.
  `init_repository` (a git write outside worktrees) stays on This Mac too.

### The app

- The page gates each project and worktree action on `project_config` or
  `worktrees` instead of "not remote". A remote Host's project menu offers
  initials, fleet, color, theme, Changes, close and remove, and new and
  remove worktree. It does not offer Show in file manager, icon, background
  or Files (the tree reads this Mac's folders until M5).
- Add project on a remote Host asks for a folder path on that Host. On This
  Mac a folder is picked, and the Host checks and adds it.
- `files_projects`, `hidden_changes_projects` and `project_themes` are
  gone. The settings come with the Host's rows, on This Mac too.

### Client settings: `client.toml`, `schema = 2` (§5 migration)

- Keys, appearance and Open with are this computer's settings, kept in
  `client.toml` beside the `projects.toml` the process uses. A private state
  keeps its own copy beside its private config.
- **Copy, never move.** The first client setting written copies the
  `[keys]`, `[appearance]` and `[open]` tables of `projects.toml` into a new
  `client.toml` with `schema = 2`, and then writes there. `projects.toml` is
  only read: never written, moved or deleted. v1 reads it as before and
  keeps the values it had at the copy.
- **Idempotent and dry-run testable.** `config::client::plan` says what
  would be written and writes nothing. `migrate` creates the file once (by
  link, never over an existing one). A `client.toml` at schema 2 is current.
  One with another or a newer schema is refused and left as it is. An
  unreadable `projects.toml` is not copied from.
- Until the copy, the app reads the client tables from `projects.toml`.
- **Not migrated here:** agents, `[skiff_agent]` and task types stay in
  `projects.toml` (M6). No real config was migrated: the tests use private
  fixtures (`skiff-core/tests/fixtures/v1/projects-1.{4,5,6}.toml`) and
  private state only.

### Tests

- **Service** (`config_writes.rs`): the same Host over this Mac's socket
  and a fake SSH forward gives the same rows and revision. Covered: a change
  and a replay of it (`conflict`), a stale writer (`conflict`, file
  unchanged), eight concurrent writers from one read (exactly one written),
  typed validation with the file unchanged, and add, inspect, arrange,
  close and remove. Worktrees: bad branches, add and duplicate, and removal
  refused for a live session, an unreachable skiffd, uncommitted changes and
  the main worktree, then forced. A Hello without the capabilities is
  refused by client and service. A stub that drops the link after a change
  sees it once, on one connection.
- **Desktop** (`config_tests.rs`): no change without a read, changes made
  from each answer's revision, and after a conflict nothing is sent until
  a read.
- **Contract** (`m4b.json`): golden requests and replies, capability per
  request, one reply kind each, a revision-less list still reads, and a
  change without a revision (or an `icon` edit) is malformed.
- **Migration** (`config::client` tests, `tests/appearance.rs`): v1.4–v1.6
  fixtures. A dry run writes nothing. Each migration matches its plan and
  leaves `projects.toml` byte for byte, so v1 reads the same config. The
  copy holds no Host key. A second run changes nothing. An edited, newer or
  unknown client file is never replaced. A bad source and a missing source
  are both handled.
- **Page:** `changes.test.ts` (This Mac and a remote Host decide Changes
  from the Host's row, and the toggle goes through `project_config`),
  `projects.test.ts` (settings come with the rows).

### Not yet

- **Image bytes:** icon and background on a remote Host. This needs an
  asset capability that carries validated bytes.
- **CLI writers:** `skiff` on a Host still edits `projects.toml` directly
  (it is the Host's own tool, under the same lock). Its `theme app` and
  `open-with` commands now write `client.toml`.
- Agent and Skipper settings (M6); the Files tree (M5); data-plane events.
- The M4 diff/recovery fixes (`c1f8f8b`) have no completed independent
  review.
- Manual two-machine and CI acceptance for M2, M3 and M4.

## M4 (additive): branches for New worktree (#166 R5)

New worktree offers the project's existing branches and a base for a new
one, on This Mac and on an SSH Host alike. The Host's git lists and checks
out the branches. The person types only the name of a new branch.

### Contract (additive, API major 1)

- **`worktrees@1`** gains `branch_list { project }`, which answers
  `branches { branches: HostBranches }`:
  - `head`: the branch the project's folder has checked out. A new branch
    starts there when `worktree_add` names no base. `null` when detached.
  - `branches`: `{ name, remote?, worktree? }`, local and remote, newest
    commit first, at most 1000. `name` is short (`main`, `origin/main`). A
    remote's `HEAD` link is left out. `worktree` is set when a worktree has
    the branch checked out, never on a remote's. False flags are left out.
- `worktree_add` gains `checkout` (left out when false). With it, `branch`
  is a local branch with no worktree, or a name exactly one remote has
  (`far` for `origin/far`). Git checks it out, and a remote's becomes a
  local branch that tracks it. `base` must be absent. The folder is
  `<dir>-worktrees/<branch>`, as for a new branch.
- `branch_list` asks for an open project; a closed or unknown one is
  `not_found`. Refused with `invalid`: `checkout` with a base, and a
  `checkout` branch that has a worktree or is not listed. A Host older than
  this slice ignores `checkout` (a new-branch add of a taken name, which git
  refuses) and does not know `branch_list`, so the page asks for it first.
- Golden shapes: `tests/fixtures/m4b.json` (`worktree_requests` and the
  `branches` replies).

### The app

- **New worktree** keeps one name field. The list under it is the branches
  with no worktree, under "Branches without a worktree". A listed name is
  checked out. Any other name is a new branch from **From:**, a menu of the
  branches that opens on `head`. From hides while the field names a listed
  branch. When `branch_list` fails, the dialog is the name field alone, as
  before.
- Audit row 5 of the #166 remote-input spec. The new branch name stays a
  typed field (in the R10 allowlist).

### Tests

- **Contract** (`m4b.json`): `branch_list` is in `worktrees@1` and answers
  `branches` only; one without `project` is malformed.
- **Service** (`config_writes.rs`, over the fake SSH forward): the list has
  the main branch with its worktree, free local branches and a remote's;
  checkout of a local branch and of a remote's; a new branch from a base;
  refusals for a branch with a worktree, an unknown one, and checkout with
  a base.
- **Page** (`newWorktree.test.ts`): the free names, From's default and
  hiding, the request for each choice, and an older Host.

## M5 (first slice): files, previews, local copies and backgrounds

Tracking: M5 child issue of [epic #155](https://github.com/solutionscay/skiff/issues/155).
`skiff-host` serves a worktree's folders and files and a project's background
as bytes. The app's Files tree, its file preview and the project background
read them from the selected Host: This Mac and an SSH Host alike. A Host
path never reaches this computer's filesystem calls or openers on a remote
Host.

### Contract (additive, API major 1)

- **`files@1`**:
  - `file_list { worktree, dir? }` answers `files { listing }`. `dir` is
    relative to the worktree; empty or absent is the worktree itself.
    `follow_links` (default false) follows links: a link to a folder is
    listed as `dir` and can be listed. The app sets it on This Mac only.
    `HostListing`: `entries` (`name`, `kind`: `dir`, `file`, `link` or
    `other`), folders first, then by name, without `.git` and what the
    Host's git ignores; `truncated` past 2000 entries that git does not
    ignore, or past 40000 read from disk.
  - `file_read { worktree, file, purpose? }` answers `file { file }`.
    `purpose` is `preview` (the default) or `copy`. `HostFile`: `content`
    (`text`, `image`, `binary`, `link`), `size`, and as they apply `mime`,
    `text`, `bytes` (base64), `truncated` and a link's `target`.
- **`project_assets@1`**: `project_background { project }` answers
  `image { image: { mime, bytes } }`. The configured name opens as a
  worktree file does: a relative one below the project folder, an absolute
  one from `/`, each component with `O_NOFOLLOW`. A link as the image is
  `invalid`; a path through a link, `..` or anything but a regular file is
  `not_found`.
- Neither family is granted by another, and neither grants a write.
  Golden shapes: `tests/fixtures/m5.json`.

### How the Host reads (`skiff_core::files`)

- **Where.** `worktree` is checked as for `git_changes` (a worktree of an
  open project, or `not_found`), and held open first (`git::Root`). The
  path must name that folder before and after. Each component below it
  opens relative to the one before, with `O_NOFOLLOW`, and a type check
  comes before any open (`git::open_beneath`). `..`, `.`, an absolute path
  and a path through a link are `not_found`.
- **Types.** A link is listed as `link` and previewed as its target. It is
  never followed (except for a listing with `follow_links`), and a copy
  of one is `invalid`. A FIFO, device or socket
  is listed as `other` and never opened (`not_found` to read). Every open
  is non-blocking.
- **Content** is decided from the first bytes, never the name. A PNG, JPEG,
  GIF, WebP, BMP or ICO signature is an image. Otherwise, as git decides, a
  NUL in the first 8000 bytes is binary, and anything else is text. SVG is
  text, so it is never drawn.
- **Bounds.**
  - text: 512 KiB, cut at a line end
  - an image to show: up to 5 MiB, sent as bytes; larger is `truncated`
    with no bytes
  - binary: no bytes
  - a copy: every byte up to 5 MiB, else `invalid` (also for a file that
    grows past the limit while it is read)
  - a listing: 2000 entries, counted after git's ignore check, from at most
    40000 read from disk. A cut listing with no entry shown is not empty.
  - git's ignore check: 10 s per folder

  Each reply fits in one 8 MiB frame.
- **Ignored files.** `git check-ignore` runs in the held folder in batches,
  with each path given as `./<path>`, so a name that starts with `:` is not
  pathspec magic. If git fails, nothing is hidden, as before. Git
  quotes a name with a control character, `"` or `\`; the Host decodes it,
  so such an ignored name is hidden like any other.
- **Backgrounds.** The Host reads the configured file (`background_file`).
  It must be a regular file of at most 5 MiB whose bytes are an image, else
  `invalid`. With no background set, the answer is `not_found`.

### The app

- **The Files tree** asks the Host (`list_files`) on This Mac and on an SSH
  Host. Show files and its key are gated on `files`, not on "not remote".
  A link row reads `name →` and does not open as a folder. A cut folder
  says how many entries show. A listing from an older data connection is
  dropped and asked again, and the data plane's recovery reads the open
  folders again.
- **Preview.**
  - **This Mac:** a regular file or a link keeps Settings › Open with on
    its own path (the Host is this Mac). A FIFO, device or socket shows
    only in the Host's viewer.
  - **Remote Host:** the peek shows the Host's preview in the page's own
    viewer (`terminal/fileView.ts`). Text shows as text, never as markup.
    An image draws from a blob of the bytes the Host sent, only for an
    image type. Binary, link, cut and too-large files say what they are.
    The folder card has no Show in file manager.
- **Open with on a remote Host** (`open_file_copy`, `src-tauri/src/copies.rs`):
  - Enter, a double-click or Open a copy fetches the file with `copy`
    (5 MiB at most).
  - The file is written as a new, read-only (`0400`) copy in
    `$TMPDIR/skiff-copies-<uid>/<pid>/<stamp>/<name>`. Every folder is
    `0700`. A shared or linked base folder is refused. On macOS the copy
    carries the quarantine flag.
  - Only the copy opens. It opens as Settings › Open with says: a
    configured `[open]` command (a terminal tool in the peek, an app
    started here), else the default app. The command gets the copy's
    path, never the Host's.
  - Types that run or install something when opened (`.app`, `.command`,
    `.pkg`, `.webloc`, `.desktop`, shell scripts and others) are refused
    before anything is fetched.
  - Each process keeps at most 32 copies. Its folder is removed when the
    app exits. Folders of processes that no longer run are removed at
    start.
  - Edits to a copy stay local. There is no sync and no remote editor.
- **Background** (`appearance/backdrop.ts`): `project_background` through
  the Host, on This Mac too. A failed read is tried again after 30 s.
  `read_image` and `list_dir` are gone: this computer has no command that
  reads a Host's image or folder by path.

### Tests

- **Core** (`files.rs`):
  - folders first, ignored files left out, a `:(glob)` name kept, a
    link and a FIFO listed by type, and listing through a link, `..` or
    a file refused
  - 2000+ entries cut, and a swapped root refused for a list and a read
  - text, image, fake `.png`, binary and link reads
  - cut text, a large image without bytes, copy limits, and a FIFO or
    `..` refused at once
  - background validation
- **Contract:** `m5.json` round trip, capability per request, one reply
  kind each, defaults, refused malformed shapes.
- **Service** (`skiff-host/tests/files.rs`, private state):
  - over this computer's socket and a fake SSH forward, `file_list` and
    `file_read` equal the core's answer
  - a 5 MiB image and copy fit a frame
  - typed refusals: `..`, absolute, through a link, a FIFO, a folder,
    missing, too large, a link copy, a closed project, a parent folder and
    `/`
  - background bytes, `invalid` for a text background, `not_found` for none
- **Real SSH** (`skiffd/tests/ssh_remote.rs`, loopback sshd): the Host's
  listing and a copy read over the data forward, and `..` refused.
- **Desktop** (`copies.rs`):
  - names keep their type, and launcher types are refused
  - copies are private, read-only, quarantined (macOS), kept to 32, and
    removed on exit
  - a dead process's copies are swept
  - a shared or linked base folder is refused, with nothing written
  - the command classification test is updated
- **Page:**
  - `files.test.ts`: both Hosts list through `files`, by worktree and
    relative folder; a stale listing is dropped and asked again; a remote
    file previews in the viewer and opens a copy; links and FIFOs never
    open; This Mac keeps Open with
  - `fileView.test.ts`: text not parsed, images only for image types,
    and every note

### Not yet (after this slice)

- **Icon and background writes on a remote Host.** Reads are done: the
  icon has been a data URL since M4, and the background now comes as
  bytes. Choosing an image stays a This Mac command on a local path. A
  remote Host needs an upload or a Host path chooser, kept Host-side.
- **Reveal on a remote Host:** there is no file manager here for a Host
  path. Copy path copies the Host path as text.
- Data-plane events: folders are read again on open, Refresh and recovery,
  not pushed.
- The M4 diff/recovery fixes (`c1f8f8b`) still have no completed
  independent review. Manual two-machine and CI acceptance for M2–M5 are
  still pending.

## M6 (first slice): the ledger, results and Skipper settings through the Host

Tracking: [#162](https://github.com/solutionscay/skiff/issues/162), child of
[epic #155](https://github.com/solutionscay/skiff/issues/155). `skiff-host`
serves Skipper's ledger, a row's result and Skipper's settings and task types
from the Host's own `ledger.db` and config. The app's Ledger panel, its
result viewer, the Skipper row and Settings › Skiff agent read them from the
selected Host: This Mac and an SSH Host alike. `skiff ledger` and the CLI on
the Host stay the only ledger writers. No schema or ledger semantics change.

### Contract (additive, API major 1)

- **`ledger@1`**:
  - `ledger_rows` answers `ledger { rows }`. Each `HostLedgerRow` is the
    panel's view of a row: ids, kind, parent, pane id and generation,
    project, task, title, state (a plan's from its tasks), times, type,
    ref, close stamp, `elsewhere` (the pane is on another skiffd than the
    Host's) and `result_exists`. The rows are the panel's: all work not
    closed, and the newest 100 closed items with their tasks, oldest
    first. A Host with no `ledger.db` answers no rows and makes none. A
    ledger older than version 7 is `invalid` and is not migrated.
  - `ledger_stamp` answers `ledger_stamp { stamp }`: an opaque value from
    the size, times and inode of `ledger.db` and its WAL. It changes when
    the ledger does. The database is not opened.
  - `ledger_result { uid }` answers `file { file }` (a `HostFile`, as
    `file_read` with `preview`). The request names a row, never a path.
- **`skipper@1`**: `skipper_settings` answers `skipper { skipper }`, a
  `HostSkipper`: name, agent, on/off, folder (or why it is refused), the
  configured folder, folder status, continue on restore and the task types
  with the command each starts on the Host.
- Neither family is granted by another or by `files`, and neither grants
  a write, a launch or watcher control. Golden shapes:
  `tests/fixtures/m6.json`.

### How the Host reads

- **Rows** come from the CLI's read code (`skiff_client::ledger::panel`):
  `ledger.db` opened read-only, the same query and plan roll-up the app
  ran before. `elsewhere` compares each row's socket with the Host's own
  skiffd socket.
- **Results.** The Host reads the row by its uid and takes the
  `result_path` the row records. It serves that file when it is below the
  folder that holds `ledger.db` (where `skiff ledger` puts results). On
  This Mac it also serves a file anywhere under the person's home folder:
  a Skipper records a repo path when its pane cannot write outside its
  repo. An SSH Host keeps the ledger folder only. The app starts This
  Mac's `skiff-host` with `--this-mac`; a service started any other way
  has the ledger-folder rule. `skiff_core::files::hold_beneath` holds the
  folder and opens each component with `O_NOFOLLOW`. A path outside the
  allowed folders, with `..`, relative, through a link, a link itself, a
  FIFO, a folder or missing is `not_found`, and `result_exists` is false
  for it. The read is a preview: 512 KiB of text at most, cut at a line
  end, an image up to 5 MiB as bytes, and no bytes for a binary file.
- **Settings** come from `skiff_client::skipper::settings`, the code the
  app ran before, on the Host's config and agent list.

### The app

- **Ledger panel** (`workspace/ledger.ts`): `ledger_rows` through
  `hostInvoke("ledger", …)`, on This Mac too. It starts after the Host's
  capabilities load. A Host without `ledger` shows why, with no rows.
  Ownership, plan grouping, task states, the workspace filter and All work
  are unchanged: they act on the same rows.
- **Change watch** (`src-tauri/src/ledger.rs`): the app asks the selected
  Host for `ledger_stamp` every 1 s (This Mac) or 3 s (a remote Host), only
  over a live session plane. It reloads the panel when the stamp changes or
  a new data connection comes up (reconnect, Host switch, restarted
  `skiff-host`). The app no longer reads `ledger.db` or its WAL.
- **Results** open in the page's own viewer (`openLedgerResult` in
  `terminal/peek.ts`), which shows the Host's bounded preview, as for a
  remote file. This is on This Mac too: no result path reaches Open with,
  an opener or a file call here.
- **Skipper row and Settings:** `get_skiff_agent` reads `skipper@1`. On a
  remote Host the Skipper row shows the Host's coordinator session (marked
  by its skiffd), and Settings › Skiff agent lists the Host's settings and
  task types read-only, with "Change it on that Host."
- **Watcher status** comes from the Host's session inventory: the watchers
  that registered with its skiffd. On a remote Host the app never starts,
  replaces or signals a watcher. It never starts or restores Skipper, and it
  hides Start watcher, Open watch log and Show in file manager. "Missing"
  says that Skiff starts watchers only on This Mac. No Host-side supervisor
  exists, and the app does not claim one: a remote watcher runs only when
  someone starts `skiff watch` on that Host.
- `ledger_rows` and `get_skiff_agent` left `LOCAL_ONLY`. `ledger_result` is
  new. The desktop `ledger.rs` has no ledger, file or env access left (the
  boundary baseline shrank).

### Demo

1. This Mac: run the dev app. Open Skipper's pane. The Ledger panel lists
   rows from `skiff-host`. A `skiff ledger` write on this Mac shows within
   a second. A done task's click shows its result in the viewer.
2. SSH Host with `skiffd` and `skiff-host` running and a Skipper session
   started there (`skiff skipper`): pick it. The Skipper row and the Ledger
   panel show that Host's coordinator and rows. Results open in the viewer.
   Settings › Skiff agent lists its task types. Nothing starts on either
   machine.

The automated equivalent is `cargo test -p skiff-host --test ledger` and
the loopback `ssh_remote` data-plane test (below).

### Tests

- **Contract:** `m6.json` round trip, capability per request, one reply
  kind each, no path in `ledger_result`, families distinct from `files`.
- **Service** (`skiff-host/tests/ledger.rs`, private state and ledger,
  over this Mac's socket and a fake SSH forward):
  - no ledger: no rows, nothing made
  - rows equal the CLI read code: plan roll-up, `elsewhere`, Finished
    capped at 100
  - results: inside, a plan's, cut at 512 KiB; a result under home only
    with `--this-mac` (`ledger_home.rs`); and refused at once: a link,
    through a folder link, a FIFO, outside, `..`, relative, missing, a
    folder, and an unknown or injected uid
  - reads leave `ledger.db` and its WAL byte for byte
  - the stamp is stable, then changes after a write
  - settings equal `skipper::settings`
  - a v6 ledger is `invalid` and unchanged
  - a restarted service is reconnected as a new instance
  - a Host without the families is refused by the client before sending
- **Real SSH** (`skiffd/tests/ssh_remote.rs`, loopback sshd, the actual
  `skiff-host` binary with a private `SKIFF_LEDGER`): rows, a result,
  an outside result refused, stamp and settings over the data forward.
- **Page** (`tests/ledger.test.ts`): rows only through `ledger`; a Host
  without it shows why; a result opens by uid in the viewer, never a path;
  settings through `skipper` on both Hosts; a remote Host never invokes
  `start_skiff_watch` or `open_skiff_agent`, and its watcher copy says who
  starts it; This Mac keeps starting its watcher and restoring Skipper.
- **Desktop:** the command classification test places `ledger_rows`,
  `ledger_result` and `get_skiff_agent` on the Host side.

### Not yet: requirements before M6 is complete

Skipper settings writes, the folder change, Skipper launch and restore, and
the watcher on the Host are done (`skipper_config@1`, `skipper_run@1` and
`skipper_run@2`, in #166 R7). Agent settings are `agent_config@1` (R8).
What is left:

- **Watch log** through the Host (today `openWatchLog` calls the local
  `skiff_watch_log`).
- **The skill sync** (`skipper::sync_skill`) on a remote Host. It stays a
  This Mac start step.
- **Change events** are polled stamps, not pushed.

The operator closed the manual two-machine runs on 2026-10-29. They are not
a requirement.

## M0 demo and handoff

M0 has zero visible behavior changes: desktop source, daemon source, protocol,
config/ledger paths and migrations are untouched. The only production connection
edit delegates the existing UnixStream split to the trait implementation that
performs the identical call. Test-only paired streams/private sockets prove the
old Ping JSON, protocol 27, ordered raw input and snapshot/output bytes still work.
Existing config/ledger fixture tests cover their unchanged on-disk contracts.

After M1g, every session-plane surface the desktop uses goes through the
selected Host, behind its own capability, and the page gates those surfaces on
the advertised capabilities. The agent list and Skipper's coordinator are the
exceptions (see M1g, Still local). Projects, Git, files, ledger and Skipper stay
local until M4–M6. The topology is decided structurally (M1f); M2 measures RTT
over a real forward.
M2–M6 own transport, services and data APIs. This document intentionally does not
implement SSH, a host process, host selection, migrations or data-location changes.

## Validation snapshot

Base: `03677f5` (v1.6.6 plus workspace fixes). M0 contract/local-parity tests,
the boundary check and negative fixtures, desktop tests/build, workspace compile
check and client/contract Clippy pass. After building the existing fixture
binaries, the workspace run passes 181 tests when excluding two pre-existing
macOS attention tests (`an_agent_title_keeps_a_quiet_turn_working` and
`an_agent_waits_at_its_prompt_and_its_turn_end_is_unread`). Both failures reproduce
from a clean `03677f5` source archive: fake agents are detected as bash. M0 does
not change foreground detection to hide this baseline failure. Linux-only suites
remain platform-gated on this Mac; no V2 Host service or installed app is launched.
