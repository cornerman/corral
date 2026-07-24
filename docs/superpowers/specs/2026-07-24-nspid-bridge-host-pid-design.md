# NSpid Bridge: Host-PID Correlation Across PID Namespaces

Status: design, 2026-07-24. Branch `nspid-bridge`.

## Problem

Focus and kill correlate an agent to its host window through a **host PID**:
`focus.rs match_pids` walks `/proc/<pid>/stat` PPid links (host `/proc`) and
matches `_NET_WM_PID`; `corral_stop_agent` and `placement::kill_pid` signal a
host PID. That PID comes today from the socket filename `<label>-<pid>.sock`
(`discovery::parse_socket_filename`, its only consumer), which the adapter fills
with a number it *assumes* is host-level.

This change also moves the PID off the filename into the record JSON. The
filename is pure addressing (a consumer reaches every socket through the
record's `socket` path, never by scanning `.corral/` or parsing the name), so
re-encoding `label` + `pid` in it is a fragile parallel channel. The record
becomes the single source of truth for `label`, `pid`, and the new
`pidNamespace`; the filename only has to be unique per directory, which
`sessionId` gives (it already names the record file `<sessionId>.json`).

The assumption breaks the moment an agent runs in a **private PID namespace**
(a hardened sandbox with `unshare-pid`): `getpid()` then returns a
namespace-local PID, unrelated to the host PID the window manager reports. The
current workaround is to disable `unshare-pid` for the agent's sandbox, which
exposes host `/proc` to the agent and lets it read other processes'
`/proc/<pid>/environ` under the same UID — a secrets leak (`PTRACE_MODE_READ_FSCREDS`
gates same-UID reads, and yama `ptrace_scope` gates only ATTACH, not READ).

A process **cannot** learn its own host PID from inside a private PID namespace:
`getpid(2)` always returns the namespace-local PID (man7 `pid_namespaces(7)`),
and a process sees only its own namespace and descendants (LWN "Namespaces in
operation, part 3"). The only sources are (a) an ancestor-namespace informant or
(b) an outer-mounted `/proc` that also exposes the leak. This is a deliberate
kernel isolation property, not a missing API.

## Rejected Alternatives

- **`CORRAL_HOST_PID` injected by the sandbox launcher (nono).** corral must not
  depend on a sandbox implementation detail; the variable would have to be
  configured for every sandboxing solution. Rejected.
- **SO_PEERCRED on the ACP socket.** The kernel translates the peer PID into the
  reader's (host) namespace, so corral would get the host PID for free from the
  connection it already opens. Works for pi/opencode (socket owner is the agent,
  terminal is its ancestor) and Cursor (extension host is a child of the Electron
  window process, so the ancestor walk reaches it). **Fails for Claude:** its
  sidecar is `detached: true` (reparented to init) and the socket filename
  deliberately carries `claudePid` (the interactive Claude process, a *different*
  process than the socket owner). SO_PEERCRED would yield the sidecar's PID, whose
  ancestor walk never reaches the Claude window. Not universal. Rejected as the
  sole mechanism.
- **`unshare-pid` off + host `/proc`.** The status quo; the leak we are removing.
  Rejected.

## Mechanism: the NSpid Bridge

The adapter advertises two scalars in the **record JSON**, both **pure self /
same-sandbox reads** (no host `/proc`):

1. `pid`: the window-owning PID **as the adapter observes it**, in its own PID
   namespace (pi/opencode: `getpid()`; Cursor: `electronPid`; Claude:
   `claudePid`) — the number that used to live in the socket filename;
2. `pidNamespace`: the identity of that PID's namespace, the **nsfs inode** of
   `/proc/<thatPid>/ns/pid` (`stat().st_ino`).

corral, running in the host (ancestor) PID namespace, sees every process in host
`/proc` regardless of namespace, and **translates** the pair to a host PID:

```
resolve_host_pid(ns_pid, pidns_ino):
    if pidns_ino == stat("/proc/self/ns/pid").st_ino:   # same ns as corral
        return ns_pid                                   # already host-level
    for hp in numeric entries of /proc:
        if stat("/proc/hp/ns/pid").st_ino != pidns_ino: continue   # wrong sandbox
        nspid = last field of the "NSpid:" line in /proc/hp/status  # deepest ns
        if nspid == ns_pid:
            return hp        # the /proc dir name IS the host pid
    return None
```

`NSpid:` lists a process's PID in each namespace it belongs to; the **leftmost**
entry is the PID in the namespace of whoever mounted the procfs (the host, for
corral), the **rightmost** the deepest (the agent's) namespace (man7
`proc_pid_status(5)`). The nsfs inode is one kernel object, so the agent's
`/proc/self/ns/pid` and corral's `/proc/<hp>/ns/pid` observe the **same**
`st_ino` — the correlation key `lsns` uses. The inode selects the sandbox; the
namespace-local PID selects the process within it.

This is universal: for Claude the sidecar reports `claudePid` +
`stat(/proc/<claudePid>/ns/pid).st_ino` (Claude and the sidecar share a PID
namespace, so both are same-sandbox reads), and corral resolves the interactive
Claude host PID even though a detached, unrelated process authored the numbers.

## Contract Change (CONVENTION.md)

New record fields, and a filename change:

| Field | Type | Meaning |
|-------|------|---------|
| `pid` | number \| null | Optional. The window-owning PID **as the agent observes it in its own PID namespace**, for window correlation. Was previously encoded in the socket filename. Absent when the agent is not window-correlatable. |
| `pidNamespace` | number \| null | Optional. The nsfs inode (`stat("/proc/<pid>/ns/pid").st_ino`) of `pid`'s PID namespace. Lets a consumer translate the namespace-local `pid` to a host PID. Absent means `pid` is already host-level (the agent runs in the consumer's PID namespace); a consumer uses it directly, preserving today's behavior. |

**Socket filename** (§3) becomes **opaque and unique per directory**, no longer
a data channel: a consumer reads `label`, `pid`, and `pidNamespace` from the
record, never from the filename. pi uses `<sessionId>.sock` (the sessionId is
known at bind time and pairs with the `<sessionId>.json` record). opencode keeps
`opencode-<pid>.sock` (it binds at plugin-load, before any session id exists),
and claude/cursor keep `<label>-<pid>.sock` (the pid there is a *foreign*
process — the interactive Claude / Electron window — not `getpid()`). All are
fine because the name is no longer parsed; only its uniqueness matters.

Backward compatible on the *reader* side: a consumer that sees no `pid` field
falls back to parsing a legacy `<label>-<pid>.sock` name during the transition,
then uses the raw PID directly when `pidNamespace` is absent (today's behavior).
Adapters in this repo all move to the new shape at once.

## Core API and Wiring

- `discovery.rs`: add `pid: Option<u32>` and `pid_namespace: Option<u64>` to
  `RegistryEntry` (parsed leniently from `pid` / `pidNamespace`). Rewrite
  `live_socket` to read the socket path plus `label`/`pid`/`pid_namespace` from
  the `RegistryEntry` instead of parsing the filename; **delete**
  `parse_socket_filename` (keep a tiny legacy fallback only if a record lacks
  `pid`). Add `resolve_host_pid(ns_pid: u32, pidns_ino: u64) -> Option<u32>`
  (the algorithm above) plus the own-ns shortcut. Pure over a `/proc` reader
  seam so it is unit-testable with a fixture tree (mirror the `ppid_of` style;
  inject a proc-root path).
- `engine.rs` (~line 129): when spawning a watcher for a newly-seen live socket,
  translate `(pid, pid_namespace)` once and store the **host PID** in the
  `SocketEntry`/`Agent`. Cache for the socket's life (the host PID is stable per
  process); do not re-translate every tick. Absent `pid_namespace` or a `None`
  result falls back to the raw `pid` (fail-open to today's behavior; focus then
  degrades loudly if the raw PID is namespace-local). Everything downstream
  (`Agent.pid`, `focus`, `placement`) is unchanged.
- `daemon` stop path: `corral_stop_agent` translates
  `(filename pid, record.pid_namespace)` before killing, so a kill lands on the
  host process. corrald already reads the record via `scan_registry`.
- `curation.rs vet`: pass `pid` and `pidNamespace` through as validated
  non-negative integers (numeric only; no injection surface). Validate the
  `socket` path resolves under `<cwd>/.corral/` as today (the filename shape is
  no longer constrained beyond that).

## Adapters (all four)

Each writes `pid` + `pidNamespace` into the record, read once at bind time via
`fs.statSync("/proc/<pid>/ns/pid").ino` (best-effort, undefined off Linux):

- `corral-pi.ts`: `pid = process.pid`; socket renamed to `<sessionId>.sock`.
- `corral-opencode.ts`: `pid = process.pid`; socket filename unchanged
  (`opencode-<pid>.sock`, bound before the session id is known).
- `corral-cursor`: `pid = electronPid` (stat its `/proc/<electronPid>/ns/pid`);
  built in the pure `lib.buildRecord`; socket filename unchanged.
- `corral-claude`: `pid = claudePid` (stat the interactive Claude's
  `/proc/<claudePid>/ns/pid`, a same-sandbox read); socket filename unchanged.

`ino` is a JS `number` from `fs.statSync`; serialized as a JSON number.
Guard the stat (a non-Linux or `/proc`-less host omits the field; correlation
then falls back to the raw PID, today's behavior). Wrap in the existing
defensive-probe style so it never throws into the host.

## Security (SECURITY.md)

`pid` and `pidNamespace` are adapter-declared, at the **same trust level as the
existing filename PID** they replace: a malicious adapter could already name any
host PID (previously in the filename, now in the record `pid`), so corral would
focus/kill that PID. The bridge adds no new surface — both are numeric, used
only to match `/proc` entries, and corral kills only on an operator action (`d`)
or a placement move. corrald re-derives the record's `cwd` from its physical
location regardless (unchanged), so the pair cannot smuggle a false directory.
Note it in SECURITY.md as adapter-controlled, equivalent to the old filename
PID.

## Sandbox (system side, coordinated separately)

With the bridge, the agent's sandbox re-enables `unshare-pid`: no host `/proc`,
no env-var leak. corral no longer depends on a shared host PID namespace for the
agent (the standing focus limitation), only on corral itself running in the host
PID namespace (unchanged — it already reads host `/proc`). This system change
lives in `~/nixos` (nono profile) and is not corral code; corral's side is
complete once translation works with `unshare-pid` on.

## E2E (nix/tests/, hard rule)

Extend the VM scenario: run an adapter under `nono` with a private PID namespace
and assert corral resolves its host PID (focus/kill correlation succeeds).
Assert the negative too: with `unshare-pid` on, the agent cannot read a host
process's `/proc/<pid>/environ` (the leak the bridge removes), keeping the
location=identity checks honest.

## Backward Compatibility and Degradation

- No `pidNamespace` → raw filename PID used directly (today's behavior).
- Translation returns `None` (kernel without `NSpid`, unreadable `ns/pid`, or a
  vanished process) → fall back to the raw PID; focus/kill fails loud if that PID
  is namespace-local, surfaced in the shell as it is today.
- Requires corral in the host PID namespace (already assumed for its host
  `/proc` walk).
