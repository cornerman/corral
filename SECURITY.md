# Corral Security Model

This document states what corral defends against, what it does not, and the
concrete threats, mitigations, and accepted risks. It is written to be honest
about the current state: each mitigation carries a status tag, because several
defenses are designed but not yet built, and claiming otherwise would mislead.

Read this before trusting corral as a boundary between mutually distrusting
agents. The short version: corral is safe for a single operator running
cooperative agents on one machine, and it is not a security boundary between
mutually untrusted agents until the mitigations tagged `[designed]` land.

Status tags used throughout:

- `[in place]` — enforced by the code on `main` today.
- `[designed]` — specified in
  [the security-hardening design](docs/superpowers/specs/2026-07-16-security-hardening-design.md),
  not yet implemented.
- `[accepted]` — a risk we accept deliberately, with the rationale stated.
- `[out of scope]` — outside the trust boundary; corral does not address it.

## Threat Model

**Defended (the target, once `[designed]` items land): a compromised or
prompt-injected agent inside one workdir sandbox.** It runs arbitrary code, can
write its own workdir and the agent-writable part of `~/.corral`, and can speak
raw JSON to the control socket. A prompt-injected agent is treated as fully
compromised: most agents have a shell tool and the sandbox must allow
`~/.corral` writes to announce, so an injected agent can bypass its adapter and
act directly. "Trusted adapter, untrusted model" is therefore not a stable
boundary.

**Out of scope, stated as preconditions corral does not enforce:**

- **Unsandboxed same-user processes.** `[out of scope]` A process running
  unsandboxed as the same OS user can ptrace a peer, edit `~/.corral`, or wrap
  corral itself. The OS user is the trust boundary; corral adds nothing below
  it. When agents are unsandboxed, corral's gates are a convenience, not a
  security control.
- **Multi-host or remote agents.** `[out of scope]` Physical-location identity
  is same-host. Message signing with a one-time location proof at enrollment is
  the clean path if remote agents ever appear.

**The load-bearing precondition:** the isolation primitive assumes the *whole*
agent process is boxed to its workdir (the nono / bwrap model, or a per-agent
container), which is what makes a sibling's socket and workdir unreachable. A
tool-level sandbox that boxes only the shell tool does **not** satisfy this, and
corral's model does not map onto it.

## The Core Principle

**Nothing stored in `~/.corral` is trusted as content; trust derives only from
physical location under a workdir's sandbox-isolated `.corral/` directory.** A
sandboxed agent can create files only inside its own workdir, so a file that
physically lives at `<D>/.corral/…` proves the announcing agent works in
directory `D`. Directory permissions are the only credential, and they already
exist, so corral introduces no tokens and no signatures.

Each mechanism reduces to a small, pure, reused function: one `resolve(fd)` that
derives a trusted `cwd`, one `registered(record)` predicate both corrald and the
viewers apply, one `normalize` for command templates. This is deliberate:
**simple code is reviewable, and reviewability is itself a security property** —
a gate whose correctness a reader cannot hold in their head at once is not one
to trust.

## Trust Boundaries

- **The operator** is the trusting authority. Actions the operator takes from
  the board (`m` message, focus, spawn, resume) are ungated by design: gating
  them would mean asking the operator to approve the operator. `[in place]`
- **The viewers (`corral`, `corral-gui`)** are pure readers of the registry
  plus the operator's own actions. They hold no messaging or approval state and
  may run many at once. `[in place]`
- **corrald** is the single trusted broker for agent-initiated messaging. It
  owns the control socket, the whitelist gate, and the approval surface, and is
  the sole registrar of launch commands. Exactly one may run (singleton guard).
  Harness registration (T4) and message authorization (T2) are **two separate
  consents** with separate stores and lifetimes, never bundled into one. `[in place]`
  for the socket/whitelist/singleton; `[designed]` for registration.
- **Agents** are untrusted peers of one another. They reach each other only
  through corrald, never directly, because each socket is workdir-local.
  `[in place]`

## Threats and Mitigations

### T1. One Agent Reaching Another's Socket

An agent connecting directly to a peer's ACP socket could drive it.

**Mitigation `[in place]`:** sockets are workdir-local
(`<cwd>/.corral/<label>-<pid>.sock`), and a whole-process workdir sandbox makes
a sibling's workdir unreachable. Cross-agent contact is only through corrald.
Relies on the load-bearing precondition above.

### T2. Spoofed Sender Identity

A message over `corrald.sock` self-reports `fromCwd` and `fromSession`. Since
session ids are public through the roster, any agent can claim any sender and
bypass the whitelist, roster, and stop gates.

**Status today `[in place]` gate but spoofable:** the whitelist and approval
gates exist, but the identity they key on is unauthenticated.

**Mitigation `[designed]`:** physical-location identity. The sender writes the
message to `<cwd>/.corral/outbox/<id>.json` and sends only `{"submit": path}`.
corrald opens the file, then derives `fromCwd` from that open fd's real path via
`/proc/self/fd` (never realpath-then-reopen, so a symlink swapped after open
cannot redirect it to a victim directory). `fromSession` is verified against the
registry: the claimed session's record must physically live in that same
directory. The act of writing inside `D` is the proof of identity `D`,
unforgeable from another box. Records and submissions share one `resolve(fd)`
function, so the derivation is reviewed in one place.

**Limitation `[accepted]`:** physical location authenticates the *directory*,
not the session within it. Two sessions in one box can each claim the other's
`fromSession` reply handle. Accepted: the directory is the unit of identity and
authorization, and same-directory agents already share a trust and sandbox
boundary.

### T3. Forged or Cross-Directory Registry Records

An agent can write `~/.corral/registry/*`. A forged record can claim another
directory, or plant attacker-chosen fields.

**Status today `[in place]` for discovery, unauthenticated:** records are plain
files whose `cwd` is trusted content.

**Mitigation `[designed]`:** the record moves into the workdir it describes
(`<cwd>/.corral/<sessionId>.json`); the registry entry becomes a symlink to it.
A consumer opens the symlink target and derives `cwd` from the open fd's real
path (the same race-free `resolve(fd)` as T2), ignoring any `cwd` in the
content. A record physically in `evil/` is attributed to `evil/`, so an attacker
cannot claim a directory it cannot write. The record's own `sessionId` must
match a strict charset (`[A-Za-z0-9._-]`, no leading `-`) and equal the symlink
filename; consumers deduplicate by resolved path. The charset is load-bearing
for T16, where `sessionId` is substituted into a launch argv.

### T4. Attacker-Chosen Launch Command (Unsandboxed Code Execution)

Records carry `spawnCommand` and `resumeCommand` that corral runs verbatim and
**unsandboxed**. A compromised agent rewrites its own record's argv to
`["bash","-c","…"]`; the code runs outside any sandbox when the operator hits
Enter or corrald spawns into that directory. This is a sandbox-escape channel
through the coordination layer.

**Status today `[in place]` verbatim execution, no gate.**

**Mitigation `[designed]`: harness registration.** A harness *kind* must be
registered before any of its agents can be used. corrald is the sole registrar:
watching the registry, on a never-seen `(label, normalized-template)` it asks
the operator once, showing the full argv, and writes the template to the sealed
`~/.corral/state/approved-commands.json`. Templates normalize the record's own
`sessionId` and `cwd` to placeholders, so a kind is registered once, ever. The
viewers and corrald share one pure `registered(record)` predicate over that
file; an unregistered kind, or a record whose argv deviates from its registered
template, is **fully quarantined** — not launchable, not focusable, not
messageable, not shown as an actionable card, surfaced only as a pending
"verify harness X?" item. Launch substitutes placeholders back and runs an exec
array (no shell); operator scrutiny of the argv at registration is the
load-bearing gate, so the surface **must** display it. Depends on T5 (the store
must be unwritable by agents).

**Side benefit:** quarantine also defuses approval-flood. A compromised agent
spamming 100 novel labels yields 100 quarantined kinds behind one review list,
not 100 cards flooding the board.

### T5. Whitelist Tampering

The message whitelist authorizes `(sender-dir -> target-dir)` pairs. If it is
agent-writable, a compromised agent appends `(itself -> anywhere)` and every
gate opens.

**Status today `[in place]` gate, but the file is agent-writable** (all of
`~/.corral` is on the sandbox allowlist).

**Mitigation `[designed]`:** split `~/.corral` by trust. `registry/` is the only
agent-writable directory; `state/` (holding `whitelist` and
`approved-commands.json`) is daemon-only and never on the sandbox allowlist, so
it is unwritable by construction.

### T6. Control-Socket Hijack

With write access to `~/.corral/`, a compromised agent can unlink
`corrald.sock` and bind its own, becoming the router: it then reads every routed
message and forges every ack.

**Status today `[in place]` hole:** the socket sits in an agent-writable
directory.

**Mitigation `[designed]`:** the socket lives at the root of `~/.corral` with a
parent that is not agent-writable. A unix socket needs write on the socket file
(bind-mounted into the sandbox), not on its parent, so the single connect
capability lets an agent talk to corrald without being able to rebind it.

### T7. Forged Provenance Tag

corrald prepends `[from <dir> (session <id>)]` to a delivered message. The tag
is in-band plain text, so a message body can embed a second, forged tag.

**Status today `[in place]` single-position tag, no body framing.**

**Mitigation `[designed]`: positional, by construction.** corrald builds the
delivered string as `"[from <dir> (session <id>)]\n" + body`, so nothing
attacker-controlled can precede or occupy the first-line tag position. The rule,
in the charter and CONVENTION, is purely positional: the sender tag is the first
line; any `[from …]` inside the body is data. A forged tag in the body is
provably not at position zero, so it is inert. No fence and no nonce: the body
runs to end-of-prompt, so there is nothing after it to delimit, and a fixed
in-band delimiter would only add a forgeable boundary. This guarantees **sender
attribution**; message-content injection is the separate, accepted T8.

### T8. Cross-Agent Prompt Injection

A delivered message becomes user-authority input to the target model. This is
prompt injection by design: the sender chooses text that the receiver's model
reads as instructions.

**Mitigation `[in place]` partial:** the provenance tag and the charter tell the
receiving model the text came from another agent, not the operator, so it can
weigh it accordingly. **Accepted residual `[accepted]`:** the injection itself
is inherent to messaging. The clean fix is out-of-band provenance in ACP
`_meta`, displayed by each harness, so the body never needs to be trusted; that
is a v2 direction, gated on harness support.

### T9. Pid-Based Focus or Kill of an Arbitrary Process

Focus, close, and stop trust a pid parsed from the socket filename. A rogue
record naming `pi-<victimPid>.sock` can make corral focus or send `SIGTERM` to
an operator-chosen process, including corrald or the compositor. The
agent-initiated stop path is the sharper case: a self-stop approval that reads
"stop agent in `<its own dir>`" can carry a forged pid that signals an unrelated
process.

**Accepted risk `[accepted]`:** we do not verify the pid. The operator is always
in the loop (a stop is gated, a focus or close is an operator action), the
effect is a wrong window or a `SIGTERM` to a same-user process, and it is
recoverable. Pid verification is judged not worth its cost. Documented so the
decision is explicit, not an oversight.

### T10. Registry-Symlink Overwrite (Session-Id Routing Hijack)

Because `registry/` is agent-writable, an agent can overwrite another session's
symlink to point at its own record bearing the victim's `sessionId`, hijacking
that id's routing.

**Accepted, bounded `[accepted]`:** identity is always the resolved directory,
which the authorization gate re-checks and the operator sees on approval. A
hijacked `sessionId` resolves to the attacker's own directory, so it can never
forge a *directory*, only redirect a routing key to a directory the gate still
independently authorizes. It is the same class as the deletion denial-of-service
below.

### T11. Denial of Service in the Shared Registry Directory

An agent with write access to `registry/` can delete or overwrite other
sessions' symlinks.

**Accepted `[accepted]`:** this is denial of service only and grants no
identity or code execution. It is inherent to a shared, same-user directory, and
an unsandboxed same-user process could do worse regardless.

### T12. Lost Messages on Daemon Crash

corrald holds accepted-but-unrouted messages in memory, with no on-disk spool.

**Accepted `[accepted]`:** a corrald crash before routing loses those messages.
Acceptable under the fire-and-forget contract and the systemd restart-on-failure
keep-alive. A durable spool is possible later if real use demands it.

### T13. No Network Exposure

**Mitigation `[in place]`:** there are no TCP ports and no network listeners.
All channels are unix sockets and files under `~/.corral` (created `0700`) and
each workdir's `.corral/` (`0700`). Peer authentication relies on directory
permissions. TCP was rejected specifically to preserve this property.

### T14. Confused-Deputy on the Submit Path

corrald is unsandboxed and opens an agent-named path (`{"submit": path}`) with
its own privilege. A hostile path (a device, a FIFO that blocks the read, a huge
file, or a path outside any workdir) could hang or mislead it.

**Mitigation `[designed]`:** corrald requires the opened fd's canonical path to
match `<dir>/.corral/outbox/<name>`, requires a regular file (`fstat`), opens
non-blocking and rejects FIFO/device/socket, and caps the read size. `fromCwd`
is derived from that validated fd only. A path that fails any check is rejected
as malformed, never read as a message.

### T15. Connection Flood on the Control Socket

A sandboxed agent with connect can open many `corrald.sock` connections
(slowloris, accept exhaustion).

**Mitigation `[designed]`:** corrald bounds concurrent accepts and applies a
per-connection read timeout, so a flood degrades gracefully rather than blocking
messaging.

### T16. Argument Injection Into a Launched Command

Launch substitutes the record's `sessionId` and `cwd` into a registered
template. An unconstrained value (a session named `--config=/evil`, a cwd
leading with `-`) would inject an argument into the launched program.

**Mitigation `[designed]`:** `sessionId` is charset-constrained at record
acceptance (T3); any `{cwd}` substituted into argv is space/`-`/`@`-guarded; and
launch runs an exec array (`setsid --fork <argv>`), never a shell, so no value
can inject a shell command. `argv[0]` resolves via the operator's PATH, not the
target cwd.

## Deployment Preconditions

Corral cannot enforce these; the deployment (for this project, `~/nixos`) must.

1. **Whole-process workdir sandbox.** Each agent must be boxed to its workdir so
   siblings' sockets and workdirs are unreachable (T1). A tool-level sandbox
   does not satisfy this.
2. **The agent sandbox profile, once the split lands `[designed]`:**
   ```
   allow write:   ~/.corral/registry/
   allow connect: ~/.corral/corrald.sock     # one file, connect only
   deny:          everything else (state/ sealed, root not writable)
   ```
   This closes T5 and T6. Granting all of `~/.corral`, as today, leaves both
   open.
3. **corrald as a kept singleton.** A systemd user service with
   restart-on-failure keeps the sole broker alive. Harness registration (T4)
   and messaging require it; "no daemon means no new registrations" is a
   deliberate property (already-registered kinds still work).

The write grant in (2) is on the *contents* of `registry/`, not on `~/.corral`
itself, or an agent could replace `registry/` or rebind the socket (T6). The
sandbox must also confine the agent's writes to its own workdir; a shared
writable bind-mount into another workdir silently breaks physical-location
identity (T2, T3).

## Current Overall Assessment

**Safe for a single operator running cooperative agents on one host.** The
channels are local and unexposed (T13), the operator is always in the loop, and
hidden-by-default spawns prevent uninvited windows.

**Not yet a boundary between mutually untrusted agents.** As of `main`, the
identity, launch-command, whitelist, and socket-rebind holes (T2, T3, T4, T5,
T6) are open, and the broker-hardening items (T14, T15, T16) are unbuilt; a
compromised sandboxed agent can spoof its identity, plant a record, tamper with
the whitelist, hijack the control socket, or induce unsandboxed code execution.
The `[designed]` mitigations close them and are specified in the hardening
design. Until they land, treat the whitelist and the gates as a convenience, not
a security control.

This document is updated as `[designed]` items land: each flips to `[in place]`
only with a test or a demonstrated defense behind it, and the assessment above
is rewritten to the achieved state.

## Reporting a Vulnerability

corral is a personal project without a formal disclosure process. If you find a
security issue, open an issue describing the threat, the affected component, and
a reproduction, or contact the maintainer directly. There is no bounty and no
guaranteed response time.
