# Corral Security Model

This document states what corral defends against, what it does not, and the
concrete threats, mitigations, and accepted risks. It is written to be honest
about the current state: each mitigation carries a status tag, because several
defenses are designed but not yet built, and claiming otherwise would mislead.

Read this before trusting corral as a boundary between mutually distrusting
agents. The short version: corral is safe for a single operator running
cooperative agents on one machine. The mitigations that make it a boundary
between mutually untrusted agents landed on `main` with the security-hardening
work (2026-07-16), each with unit tests behind it.

Status tags used throughout:

- `[in place]` — enforced by the code on `main` today, with a test behind it.
- `[designed]` — specified in
  [the security-hardening design](docs/superpowers/specs/2026-07-16-security-hardening-design.md),
  not yet implemented.
- `[accepted]` — a risk we accept deliberately, with the rationale stated.
- `[out of scope]` — outside the trust boundary; corral does not address it.

## Threat Model

**Defended: a compromised or
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
  security control. This includes **corrald itself**, which runs unsandboxed
  today — a parsing bug in the one process that reads every untrusted
  record/message is full-authority RCE. Confining the broker (systemd unit
  hardening, or a dedicated OS user owning `state/`) is a `[designed]`
  blast-radius reduction, not a new boundary; see TODO.md "Confine the broker".
  Being out of scope here is not a regression from adding corral — see "Adoption
  Is Risk-Neutral" below.
- **Multi-host or remote agents.** `[out of scope]` Physical-location identity
  is same-host. Message signing with a one-time location proof at enrollment is
  the clean path if remote agents ever appear.

**The load-bearing precondition:** the isolation primitive assumes the *whole*
agent process is boxed to its workdir (the nono / bwrap model, or a per-agent
container), which is what makes a sibling's socket and workdir unreachable. A
tool-level sandbox that boxes only the shell tool does **not** satisfy this, and
corral's model does not map onto it.

**Fails silently, so it is the deployment's job to guarantee.** Every identity
gate below derives from "a file under `<D>/.corral/` was written by an agent
boxed to `D`." If that boxing is wrong — an unsandboxed agent, a tool-level
sandbox, or a shared writable bind-mount that lets one agent write into another
workdir — physical-location identity is void, but **nothing errors**: corrald
sees only where files live, not how each agent is jailed, so it cannot detect
the breach and keeps curating and routing as if identities were sound. There is
no in-band signal and no fail-loud path corral can offer for the sandbox itself
(the one thing corrald *can* check is its own `state/` permissions). Treat the
sandbox profile in Deployment Preconditions as security-critical: a
misconfiguration downgrades every gate from a boundary to a convenience without
announcing it.

## Adoption Is Risk-Neutral

Turning corral on grants an agent no privilege it does not already have, so it
never moves you to a worse security position — whether or not you sandbox.

- **If your agents already run unsandboxed** (as most do), they can already read
  and write every directory, run arbitrary code as you, and reach the network.
  corral adds a broker running as the same user that parses agent input — but
  its socket is `0700`, reachable only by same-user processes, and a same-user
  unsandboxed peer already has full power, so corrald is no new privilege, only
  new code reachable solely by processes that are already all-powerful. What you
  gain is pure: visibility of every session and operator-gated cross-project
  messaging. (Functionally, plain subagents would already suffice in this mode;
  corral adds the board.)
- **If your agents are sandboxed** to their workdirs, corral is the *controlled*
  channel that lets otherwise-isolated projects talk, with the operator gating
  each `(sender -> recipient)` pair. The isolation you built is preserved; corral
  only opens the specific, approved holes.

The one channel corral genuinely introduces is cross-agent prompt injection
(T8) — but it is operator-gated per pair and strictly weaker than what a
co-resident unsandboxed process can already do to a peer (ptrace, overwriting
its files, killing it). So sandboxing is the idea corral embraces, not a tax it
imposes: skipping it loses you nothing corral was protecting.

## The Core Principle

**Nothing stored in `~/.corral` is trusted as content; trust derives only from
physical location under a workdir's sandbox-isolated `.corral/` directory.** A
sandboxed agent can create files only inside its own workdir, so a file that
physically lives at `<D>/.corral/…` proves the announcing agent works in
directory `D`. Directory permissions are the only credential, and they already
exist, so corral introduces no tokens and no signatures.

Each mechanism reduces to a small, pure, reused function, and they all run in
one process (corrald, the curator): one `resolve(fd)` deriving a trusted `cwd`,
one `registered(record)` predicate, one `normalize` for command templates. The
viewers hold none of it — they read only corrald's vetted output. This is
deliberate:
**simple code is reviewable, and reviewability is itself a security property** —
a gate whose correctness a reader cannot hold in their head at once is not one
to trust.

## Trust Boundaries

- **The operator** is the trusting authority. Actions the operator takes from
  the board (`m` message, focus, spawn, resume) are ungated by design: gating
  them would mean asking the operator to approve the operator. `[in place]`
- **The viewers (`corral`, `corral-gui`)** read **only** the vetted
  `~/.corral/state/registry/` that corrald produces, plus the operator's own
  actions. They do no authentication, resolution, or filtering of their own, so
  no agent-writable record ever reaches a viewer. They still watch live sockets
  directly for state/activity (low-severity display). Many may run at once.
  `[in place]` (both shells read only `paths::state_registry_dir()`).
- **corrald** is the single trusted broker and the **registry curator**. It is
  the only reader of the agent-writable raw index; it authenticates, validates
  every field, and emits the vetted, sealed `state/registry/` the viewers read
  ("parse, don't validate": untrusted records in, trusted records out). It also
  owns the control socket, the whitelist gate, the approval surface, and is the
  sole registrar of launch commands. Exactly one may run (singleton guard).
  Harness registration (T4) and message authorization (T2) are **two separate
  consents** with separate stores and lifetimes, never bundled into one. `[in place]`.
  **Tradeoff:** viewers now need corrald for discovery (no daemon ⇒ an empty
  board). Accepted for the trust concentration: the entire identity/argv attack
  surface lives in one process, and viewers render only sealed data.
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

**Before this hardening: gate but spoofable:** the whitelist and approval
gates exist, but the identity they key on is unauthenticated.

**Mitigation `[in place]`:** physical-location identity. The sender writes the
message to `<cwd>/.corral/outbox/<id>.json` and sends only `{"submit": path}`.
corrald opens the file, then derives `fromCwd` from that open fd's real path via
`/proc/self/fd` (never realpath-then-reopen, so a symlink swapped after open
cannot redirect it to a victim directory). `fromSession` is verified against the
curated registry: the claimed session must be one corrald already authenticated
to that same directory. The act of writing inside `D` is the proof of identity
`D`, unforgeable from another box.

**Limitation `[accepted]`:** physical location authenticates the *directory*,
not the session within it. Two sessions in one box can each claim the other's
`fromSession` reply handle. Accepted: the directory is the unit of identity and
authorization, and same-directory agents already share a trust and sandbox
boundary.

### T3. Forged or Cross-Directory Registry Records

An agent can write `~/.corral/registry/*`. A forged record can claim another
directory, or plant attacker-chosen fields.

**Before this hardening: for discovery, unauthenticated:** records are plain
files whose `cwd` is trusted content.

**Mitigation `[in place]`: corrald curation.** Records live per-project at
`<D>/.corral/registry/<sessionId>.json`; a newline-delimited `~/.corral/registry`
file indexes the directories `D`. corrald is the only reader: it canonicalizes
each `D` (race-safe, from a directory fd), attributes every record it finds
there to that `D`, and **ignores any `cwd` in the content — the record does not
carry one**. A record physically under `evil/` is attributed to `evil/`, so an
attacker cannot claim a directory it cannot write; adding a dir to the index
only surfaces records that genuinely exist there. Being in the right directory
authenticates only *who wrote it* — **every other field is still adversarial**
(see T4, T16, T17), so corrald validates each before emitting the vetted record.
The `sessionId` must match a strict charset (`[A-Za-z0-9._-]`, no leading `-`)
and equal the filename; corrald deduplicates by resolved path.

### T4. Attacker-Chosen Launch Command (Unsandboxed Code Execution)

Records carry `spawnCommand` and `resumeCommand` that corral runs verbatim and
**unsandboxed**. A compromised agent rewrites its own record's argv to
`["bash","-c","…"]`; the code runs outside any sandbox when the operator hits
Enter or corrald spawns into that directory. This is a sandbox-escape channel
through the coordination layer.

**Before this hardening: verbatim execution, no gate.**

**Mitigation `[in place]`: harness registration.** A harness *kind* must be
registered before any of its agents can be used. corrald is the sole registrar:
watching the registry, on a never-seen `(label, normalized-template)` it asks
the operator once, showing the full argv, and writes the template to the sealed
`~/.corral/state/approved-commands.json`. A template pins the **whole
launch-affecting set** — `spawnCommand`, `resumeCommand`, `gui`, and
`messageFlag` — with the record's own `sessionId` normalized to a placeholder,
so a kind is registered once but **any change to any of those fields is a new
unique set that needs its own approval**. The prompt shows the entire set, not
just the argv, so a flip of `gui` (terminal vs direct launch) or `messageFlag`
cannot ride in under an already-approved command. corrald applies one pure
`registered(record)` predicate when curating; an unregistered kind, or a record
whose set deviates, is **fully quarantined** — it is never written to
`state/registry`, so viewers cannot see, focus, message, or launch it; it
surfaces only as a pending "verify harness X?" item on corrald's tray. Launch
substitutes the placeholder back and runs an exec array (no shell); operator
scrutiny of the shown set at registration is the load-bearing gate. Depends on
T5 (the store must be unwritable by agents).

Only a *new or deviating* set prompts: a new session of an already-registered,
matching kind is admitted to `state/registry` silently, so registration is
per-kind-and-command-shape, never per session.

**Side benefit:** quarantine also defuses approval-flood. A compromised agent
spamming 100 novel labels yields 100 quarantined kinds behind one review list,
not 100 cards flooding the board.

### T5. Whitelist Tampering

The message whitelist authorizes `(sender-dir -> target-dir)` pairs. If it is
agent-writable, a compromised agent appends `(itself -> anywhere)` and every
gate opens.

**Before this hardening: gate, but the file is agent-writable** (all of
`~/.corral` is on the sandbox allowlist).

**Mitigation `[in place]`:** split `~/.corral` by trust. The only agent-writable
surface is the `~/.corral/registry` dir-index file (append) and the socket
(connect); `state/` (holding `whitelist`, `approved-commands.json`,
`state/registry/`, and `audit.log`) is daemon-only and never on the sandbox
allowlist, so it is unwritable by construction.

### T6. Control-Socket Hijack

With write access to `~/.corral/`, a compromised agent can unlink
`corrald.sock` and bind its own, becoming the router: it then reads every routed
message and forges every ack.

**Before this hardening: hole:** the socket sits in an agent-writable
directory.

**Mitigation `[in place]`:** the socket lives at the root of `~/.corral` with a
parent that is not agent-writable. A unix socket needs write on the socket file
(bind-mounted into the sandbox), not on its parent, so the single connect
capability lets an agent talk to corrald without being able to rebind it.

### T17. Record Aiming corral at Another Session's Socket

A record's `socket` field is attacker-authored content. An agent's own record
(honestly attributed to its dir `D`) could set `socket` to *another* session's
socket path; corral would then connect there to watch and, worse, the operator's
ungated `m` to that card would deliver into the **victim's** session.

**Mitigation `[in place]`:** corrald requires the record's `socket` to resolve
**inside `<D>/.corral/`** (the record's own authenticated directory) before
emitting the vetted record; a socket pointing elsewhere is rejected. So a card
can only ever drive a session in its own box. **Future `[designed]`:** an
install-time nonce the socket proves on connect, to authenticate the socket
itself and not just its path location.

### T18. Viewer Parses the Live Socket Stream

Viewers connect to each live agent socket to watch its live state
(running/idle/requires_action, activity, title). So a viewer parses one
untrusted stream from a possibly-compromised agent. The record's identity is
already authenticated (the card's `cwd`/socket came from corrald's vetted
`state/registry/`), so the worst a hostile stream achieves is **display**
spoofing of its own card: a wrong state dot, a misleading activity line, a
forged title — on a card that is already, correctly, attributed to that box.

**Accepted residual `[accepted]` (decision B, 2026-07-16):** display-only, on an
already-authenticated card, no identity or code-execution gain. The principled
fix is **full socket mediation** — corrald becomes the sole socket connector,
folds live state into `state/registry/`, and viewers read everything from there
(zero untrusted input in a viewer). Deferred because it deepens the corrald
dependency and routes operator `m` / card-move cancel-nudge through corrald;
tracked in TODO.md (Future Features). The inotify watch on `state/registry/` is
already the viewer mechanism that migration would use.

### T7. Forged Provenance Tag

corrald prepends `[from <dir> (session <id>)]` to a delivered message. The tag
is in-band plain text, so a message body can embed a second, forged tag.

**Before this hardening: single-position tag, no body framing.**

**Mitigation `[in place]`: positional, by construction.** corrald builds the
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

### T10-note. Symlink Overwrite Eliminated by the Curator Model

An earlier design put per-session symlinks in an agent-writable
`~/.corral/registry/`, which an agent could overwrite to hijack another
session's id. The curator model removes that vector: there are no per-session
symlinks. The only shared agent-writable surface is the `~/.corral/registry`
*index file* of directories; appending to it merely points corrald at a
directory whose records are still authenticated by physical location. There is
nothing to overwrite for identity gain.

### T11. Denial of Service in the Shared Index

An agent can append junk to, or truncate, the `~/.corral/registry` index file.

**Accepted `[accepted]`:** this is denial of service only (at worst, discovery
is delayed until live agents re-append their dirs) and grants no identity or
code execution. It is inherent to a shared, same-user file, and an unsandboxed
same-user process could do worse regardless. corrald ignores unparsable or
nonexistent index lines.

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

**Mitigation `[in place]`:** corrald requires the opened fd's canonical path to
match `<dir>/.corral/outbox/<name>`, requires a regular file (`fstat`), opens
non-blocking and rejects FIFO/device/socket, and caps the read size. `fromCwd`
is derived from that validated fd only. A path that fails any check is rejected
as malformed, never read as a message.

### T15. Connection Flood on the Control Socket

A sandboxed agent with connect can open many `corrald.sock` connections
(slowloris, accept exhaustion).

**Mitigation `[in place]`:** corrald bounds concurrent accepts and applies a
per-connection read timeout, so a flood degrades gracefully rather than blocking
messaging.

### T16. Argument Injection Into a Launched Command

Launch substitutes the record's `sessionId` and `cwd` into a registered
template. An unconstrained value (a session named `--config=/evil`, a cwd
leading with `-`) would inject an argument into the launched program.

**Mitigation `[in place]`:** `sessionId` is charset-constrained at record
acceptance (T3); any `{cwd}` substituted into argv is space/`-`/`@`-guarded; and
launch runs an exec array (`setsid --fork <argv>`), never a shell, so no value
can inject a shell command. `argv[0]` resolves via the operator's PATH, not the
target cwd.

## Audit Trail

corrald appends every security-relevant decision to a sealed, append-only
`~/.corral/state/audit.log` (daemon-written, in the agent-unwritable `state/`):
harness registration (approved / denied, with the shown launch set), message
authorization (allow-once / allow-always / deny), stops, deliveries, and records
it quarantined (with the reason). Each line carries a timestamp and the
directories / sessions involved. `[in place]` (the log is written by
`curator::audit`); a tray "open audit log" action is `[designed]`, not yet
built — the file is opened by hand today.

A file is chosen over a bespoke UI: it is durable across restarts, greppable,
and trivially reviewed, and it cannot itself become an attack surface. The log
is the operator's after-the-fact record of what the broker did on their behalf.

## Deployment Preconditions

Corral cannot enforce these; the deployment (for this project, `~/nixos`) must.

1. **Whole-process workdir sandbox.** Each agent must be boxed to its workdir so
   siblings' sockets and workdirs are unreachable (T1). A tool-level sandbox
   does not satisfy this.
2. **The agent sandbox profile (deployment glue in `~/nixos`, `[designed]`):**
   ```
   allow append:  ~/.corral/registry          # the dir index (one file)
   allow connect: ~/.corral/corrald.sock       # one file, connect only
   deny:          everything else (state/ sealed, no other ~/.corral writes)
   ```
   Records live in the agent's own workdir (`<D>/.corral/registry/`), already
   writable as part of the workdir; no `~/.corral` grant is needed for them.
   The agent touches only two things under `~/.corral`: append its dir to the
   index, and connect to the socket. This closes T5 and T6.
3. **corrald as a kept singleton.** A systemd user service with
   restart-on-failure keeps the sole broker alive. Harness registration (T4)
   and messaging require it; "no daemon means no new registrations" is a
   deliberate property (already-registered kinds still work).

The append grant in (2) is on the `~/.corral/registry` index file only, not on
`~/.corral` itself, or an agent could rebind the socket (T6). The sandbox must
also confine the agent's writes to its own workdir; a shared writable bind-mount
into another workdir silently breaks physical-location identity (T2, T3).

## Current Overall Assessment

**Safe for a single operator running cooperative agents on one host.** The
channels are local and unexposed (T13), the operator is always in the loop, and
hidden-by-default spawns prevent uninvited windows.

**A boundary between mutually untrusted agents — in place.** The identity,
launch-command, whitelist, socket-rebind, provenance, and broker-hardening holes
(T2–T7, T14–T17) are closed on `main`, each with unit tests behind it:
physical-location identity (corrald curates a vetted registry from per-workdir
records and outbox submissions), harness registration (no unapproved launch
command runs), the `state/` seal (whitelist, approved-commands, vetted registry,
audit log unwritable by agents), the root-socket (no rebind), the positional
provenance tag, and the broker's confused-deputy + flood defenses.

The residuals that remain are deliberate: cross-agent message content is prompt
injection by nature (T8, fix is out-of-band `_meta`, v2); pid-based focus/kill
is unverified by choice (T9); a viewer still parses the live socket stream for
display (T18, fixed by full mediation, deferred — TODO.md); and everything rests
on the whole-process workdir sandbox precondition (deployment glue, not corral
code). None is a code-execution or cross-directory-identity hole.

## Reporting a Vulnerability

corral is a personal project without a formal disclosure process. If you find a
security issue, open an issue describing the threat, the affected component, and
a reproduction, or contact the maintainer directly. There is no bounty and no
guaranteed response time.
