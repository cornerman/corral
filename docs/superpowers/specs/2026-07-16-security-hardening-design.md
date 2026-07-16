# Security Hardening: Authenticated Identity, Trusted Launch Commands

Status: design (2026-07-16). Approved for planning. The operator-facing summary
of the resulting threat model, mitigations, and accepted risks lives in
[SECURITY.md](../../../SECURITY.md); this document is the implementation design
behind it.

## Summary

Corral's inter-agent layer trusts data that a compromised agent controls. This
design closes the four holes the code review found, under one principle:
**nothing stored in `~/.corral` is trusted as content; trust derives only from
physical location under a workdir's sandbox-isolated `.corral/` directory.**

The four findings and their fixes:

1. **Spoofable sender identity.** `fromCwd` / `fromSession` are self-reported
   over `corrald.sock`, so the whitelist, roster, and stop gates are
   bypassable. Fix: physical-location identity — a symlinked registry and an
   outbox-file submission, so a claim of "I act in directory D" is proven by the
   act of writing inside D, which only D's sandboxed agent can do.
2. **Attacker-chosen launch argv.** Registry records carry `spawnCommand` /
   `resumeCommand` that corral runs verbatim and unsandboxed. Fix: a
   trust-on-first-use store of approved command templates, with corrald as the
   sole approver and the viewers reading it.
3. **Forgeable provenance.** The `[from …]` tag is in-band plain text. Fix: a
   positional rule (the first tag is authentic) plus a fenced body.
4. **Unverified pid kills.** Focus and kill trust a pid parsed from a socket
   filename. Decision: accept the risk (documented), no verification, plus a
   small cleanup of the `classify` gate.

A directory split of `~/.corral` supports findings 1 and 2 and closes two holes
of its own (whitelist tampering and control-socket hijack).

**Design constraint: simple code is reviewable, and reviewability is a safety
property.** Each mechanism reduces to a small, pure, unit-tested function reused
by every caller: one `resolve` that derives a trusted `cwd` from an open fd
(records and submissions alike), one `registered` predicate that both corrald
and the viewers apply to filter the registry, one `normalize` for command
templates. No mechanism whose correctness a reviewer cannot hold in their head
at once.

## Threat Model and Scope

**Defended:** a compromised or prompt-injected agent running arbitrary code
inside one workdir sandbox. It can write its own workdir, write the
agent-writable part of `~/.corral`, and speak raw JSON to `corrald.sock`. A
prompt-injected agent is treated as fully compromised, because most agents have
a shell tool and the sandbox must allow `~/.corral` writes to announce, so an
injected agent can bypass its adapter and act directly.

**Out of scope, documented as preconditions corral does not enforce:**

- Unsandboxed same-user processes. Such a process can ptrace a peer, edit
  `~/.corral`, or wrap corral itself. The OS user is the trust boundary; corral
  adds nothing below it. Corral's gates are a convenience, not a security
  control, when agents are unsandboxed.
- Multi-host or remote agents. Physical-location identity is same-host.
  Message signing with a one-time location proof at enrollment is the clean v2
  path if remote agents ever appear.
- **Accepted risk (finding 4):** a rogue record's pid causes corral to focus or
  send `SIGTERM` to an operator-chosen process, possibly corrald or the
  compositor. Accepted because the operator is always in the loop, the effect
  is a wrong window or process, and it is recoverable. Not worth pid
  verification.

**The dependency to state loudly:** the isolation primitive assumes the whole
agent process is boxed to its workdir (the nono / bwrap model or a per-agent
container), which is what makes a sibling's socket and workdir unreachable. A
tool-level sandbox that boxes only the shell tool does not satisfy this.

## The Core Principle: Physical Location Is Identity

A sandboxed agent can create files only inside its own workdir. So a file that
physically lives at `<D>/.corral/…` is proof that the agent announcing it works
in directory `D`. Corral already relied on this for socket isolation; this
design applies it consistently to identity and submission. Directory
permissions are the only credential, and they already exist, so no tokens and
no signatures are introduced.

## Mechanism 1: Physical-Location Identity

### Symlinked Registry (Authenticates Announcements)

The record moves into the workdir it describes; the registry becomes an index
of symlinks pointing at those records.

- The agent writes the real record to `<cwd>/.corral/<sessionId>.json`
  (atomic: temp file, then rename), beside its socket.
- The agent creates `~/.corral/registry/<sessionId>.json` as a **symlink** to
  that real file.
- A consumer **opens** the symlink target, then derives `cwd` from that open
  file descriptor's real path via `/proc/self/fd/<n>`, requiring the path to
  match `<cwd>/.corral/<sessionId>.json` and stripping the suffix. It reads the
  record content **from the same fd**. It **ignores any `cwd` field in the
  content**; the physical location is authoritative.
- **`sessionId` MUST match a strict charset** (`[A-Za-z0-9._-]`, no leading
  `-`) and equal the symlink filename; a record violating either is rejected.
  This is load-bearing for Mechanism 3, where `sessionId` is substituted into a
  launch argv (finding C3): a value like `--config=/evil` would otherwise inject
  an argument into the launched command.

**Never `realpath`-then-reopen.** Resolving to a path string and opening it
again is a time-of-check/time-of-use race: an agent swaps the symlink between
the two steps, so `realpath` returns a victim-looking path (cwd = victim) while
the reopen reads the attacker's file, attributing attacker content to the
victim's directory. Opening once and deriving `cwd` from that same fd closes
it. Records and outbox submissions share **one** `resolve(fd) -> cwd` function,
both for correctness and so a reviewer checks the derivation in one place.

The registry stays the single discovery store: a consumer scans the symlink
farm, never the scattered workdirs.

Attacker analysis (agent boxed to `evil/`):

- A symlink into `victim/.corral/` at a file the attacker authored: impossible,
  the attacker cannot write there.
- A symlink at the victim's existing record under a new name: a duplicate card
  at worst; the content is the victim's honest record, no authority is gained.
  Consumers deduplicate by resolved path and require the record's own
  `sessionId` to equal the symlink filename.
- A symlink chain that looks like the victim but physically resolves into
  `evil/` or `/tmp`: canonicalization derives `evil/` or `/tmp` as the cwd, so
  the record is attributed honestly to the attacker.
- Overwriting or deleting another session's symlink in `~/.corral/registry/`:
  denial of service only, already possible in a shared directory, and it grants
  no identity. See the accepted edge below.

### Outbox-File Submission (Authenticates Messages)

A message over `corrald.sock` still only claims its sender today. The same
principle authenticates it: the sender proves write capability in its own
workdir.

- The sender writes the message JSON to `<cwd>/.corral/outbox/<id>.json`.
- The sender sends only `{"submit": "<absolute path to that file>"}` over
  `corrald.sock`.
- corrald opens the file, resolves its canonical physical path race-free (open
  the fd, then read `/proc/self/fd/<n>`, so a symlink swapped after the open
  cannot redirect it), and **derives `fromCwd`** from the physical location,
  exactly as for a record. It ignores any `fromCwd` in the file content.
- `fromSession` is verified against the now-authoritative registry: the claimed
  session's record must physically live in the derived `fromCwd`.
- corrald reads the file, then deletes it. The ack stays synchronous on the
  socket (`accepted` / `approval_needed` / `recipient_not_found` /
  `directory_not_known` / `malformed`).

One rule explains both halves: nothing in `~/.corral` is trusted as content;
only physical location under a workdir's `.corral/` is.

### Accepted Edge: Registry-Symlink Overwrite

Because `~/.corral/registry/` is agent-writable, an agent can overwrite another
session's symlink to point at its own record bearing the victim's `sessionId`,
hijacking that id's routing. This is bounded and accepted: **identity is always
the resolved directory**, which the authorization gate re-checks and the
operator sees on approval. A hijacked `sessionId` resolves to the attacker's own
directory, so it can never forge a *directory*, only redirect a routing key to a
directory the gate still independently authorizes. It is the same class as the
deletion denial-of-service already possible in a shared directory.

## Mechanism 2: The Sandbox Surface (Directory Split)

Every agent runs as the same OS user, so file permissions isolate nothing here;
the sandbox profile is the only real boundary. The job is to make that profile
a clean, self-documenting directory rule. The layout:

```
~/.corral/
  corrald.sock           # root-level: agents connect, corrald binds
  registry/              # PUBLIC  — agent-writable (session symlinks)
  state/                 # PRIVATE — daemon-only (whitelist, approved-commands.json)
```

The agent sandbox profile becomes three self-evident lines:

```
allow write:   ~/.corral/registry/
allow connect: ~/.corral/corrald.sock     # one file, connect only
deny:          everything else (state/ fully sealed, root not writable)
```

This closes two holes the current "grant all of `~/.corral`" opens:

- **Whitelist tampering.** A compromised agent today appends
  `(itself -> anywhere)` to `~/.corral/whitelist` and every gate opens. With
  `whitelist` in the sealed `state/`, it is unwritable by construction.
- **Control-socket hijack.** With write access to `~/.corral/`, an agent today
  can unlink `corrald.sock` and bind its own, becoming the router: it then
  reads every routed message and forges every ack. The socket at the root, with
  its parent not agent-writable, cannot be rebound. A unix socket needs write on
  the socket file (bind-mounted in), not on its parent, so the single connect
  capability does not reopen the hole.

The connect line is not extra structure; it is the one channel that makes
agent-to-corrald messaging exist at all. `state/` is never exposed to an agent.

**Profile precision (finding M4).** The write grant is on the *contents* of
`registry/`, not on `~/.corral` itself: if the root were writable, an agent
could replace the `registry/` directory or unlink and rebind `corrald.sock`,
reopening the hijack. The whole model also presumes the sandbox confines the
agent's writes to its own workdir and `registry/` only; a shared writable
bind-mount into another workdir silently breaks physical-location identity, so
the deployment MUST NOT create one.

**Enforcement boundary.** Corral cannot apply the sandbox profile itself.
CONVENTION states the requirement; the `~/nixos` deployment glue implements it
(bwrap / landlock). `core::paths` moves the default layout here; the existing
env overrides (`CORRAL_REGISTRY_DIR`, `CORRAL_CONTROL_SOCKET`,
`CORRAL_WHITELIST`) stay, and a new `CORRAL_STATE_DIR` (or equivalent) covers
the approved-commands store.

## Mechanism 3: Harness Registration (Approved Command Templates)

Physical-location identity stops a record from lying about *whose* it is, but a
compromised agent can still rewrite *its own* record's argv to
`["bash","-c","…"]`, and that argv runs unsandboxed when the operator or corrald
launches it. The fix: **a harness kind must be registered before any of its
agents can be used.** corrald is the sole registrar; the viewers only ever
consume registered kinds.

### Registration Is the Gate to Usability

The unit is the `label` (harness kind) plus its normalized command templates.
corrald assures registration: watching the registry, on a first-seen `(label,
template)` it raises **one** "register harness `X`?" approval showing the
literal `spawnCommand` / `resumeCommand` argv, and on approval writes the
template to the sealed store. From then on every agent of that label whose
commands match is a full citizen.

An **unregistered** kind (or a record whose argv deviates from its label's
registered template) is **fully quarantined**: not launchable, not focusable,
not messageable, and not shown as an actionable card. It surfaces only as the
single pending "verify harness `X`?" item. So a compromised agent spamming 100
novel labels yields 100 quarantined kinds behind one review list, not 100 cards
flooding the board (this is the finding-H2 approval-flood fix, falling out of
registration rather than a bolt-on rate limiter). The cost is a one-time
ceremony: the first session of a genuinely new harness waits as "pending
verification" for the seconds until the operator registers the kind, then it and
all siblings are full citizens. That cost buys the property that **nothing
attacker-authored reaches the actionable board until the operator has vetted the
kind.**

### Store

- File: `~/.corral/state/approved-commands.json`, in the daemon-owned `state/`
  directory, so it is **not agent-writable**. This dependency on Mechanism 2 is
  load-bearing: without the sealed directory a compromised agent would
  pre-write its own approval.
- Shape: one normalized template per `(label, mode)`:

```json
{
  "pi":       { "spawn": ["pi"],       "resume": ["pi","--session","{sessionId}"] },
  "opencode": { "spawn": ["opencode"], "resume": ["opencode","--session","{sessionId}"] },
  "cursor":   { "spawn": ["cursor","{cwd}"], "resume": ["cursor","{cwd}"] }
}
```

### Normalization

Pinning argv verbatim would re-prompt on every new session (each has a unique
id). Before comparing or storing, corral substitutes the record's own known
variable values with placeholders: an arg equal to the record's `sessionId`
becomes `{sessionId}`, an arg equal to its `cwd` becomes `{cwd}`. Every pi
session then collapses to one template. Launch reverses the substitution to
build the real argv. An attacker who buries `bash -c evil` in any non-placeholder
position produces a different normalized template, so it never matches.

### One Shared Predicate: corrald Registers, Viewers Filter

corrald is the sole approver and the only writer of the store. The viewers
(`corral`, `corral-gui`) only **read** it and apply **one shared pure
predicate** — `registered(record)`: does the record's normalized `(label,
template)` match a stored entry? — to filter the registry. A record that fits is
taken as a full citizen; one that does not is quarantined. corrald applies the
same predicate to decide what still needs registration and what is routable. No
viewer talks to corrald; both sides just apply the same function over the sealed
`state/` file, so enforcement cannot drift and a reviewer checks it once.

Because corrald registers eagerly at announce time, a kind is usually already
registered before the operator reaches for it.

### Argv Substitution Is Exec-Safe

Launch builds the real argv by substituting `{sessionId}` and `{cwd}` back into
the registered template and running it as an exec array (`setsid --fork
<argv>`), never through a shell, so a value can never inject a *shell* command.
Argument injection is closed upstream: `sessionId` is charset-constrained
(Mechanism 1), and any `{cwd}` substituted into argv is space/`-`/`@`-guarded as
the message path already guards. `argv[0]` resolves via the operator's PATH, not
the target cwd. The registered template pins `gui` and `messageFlag` too, so a
post-registration flip of those record fields cannot change launch behavior
under an otherwise-matching argv.

### Registration and Messaging Are Two Separate Approvals (finding H3)

They are distinct trust objects with distinct stores and lifetimes, never one
consent:

- **Registration** is a property of the harness *kind* → `approved-commands.json`,
  permanent per `(label, template)`.
- **Message authorization** is a property of the *directory pair* → `whitelist`,
  once or always.

They are decoupled at the source: registration fires from the registry-watch at
announce time, message authorization fires per pair at message time. An
agent-initiated spawn-delivery proceeds only when **both** are independently
satisfied. Denying registration quarantines the kind but leaves the pair's
verdict untouched; denying the message blocks delivery but the registration (if
granted) persists (trusting `pi` is fine regardless of one message). They
surface as two separate lists in corrald's pull-based review surface; neither
approval is ever bundled into or implied by the other.

### Safety Rests on the Operator Reading the Argv (finding M2)

The `label` is attacker-controlled content, so an attacker can claim
`label: "pi"` with evil argv. If pi is already registered, the evil argv
deviates and is quarantined silently. If it is first-seen (a malicious first
registration, or a novel label like `pi2`), the prompt shows the literal argv,
so the operator rejects it. The registration surface **must** display the full
argv; this is a hard requirement. corral cannot distinguish a benign command
from a plausible-looking malicious one, so operator scrutiny at registration is
the load-bearing gate. A known-binary allowlist was considered and rejected: it
would break the convention's harness-neutrality (adapters self-describe their
launch).

### Consequence

Registration requires corrald to be running, since it is the sole registrar.
Without the daemon no *new* kind can be registered, so an unregistered kind
stays quarantined; already-registered kinds remain full citizens (the viewer
just reads the file). Since corrald is the systemd-kept singleton this threat
model is built around, "no daemon means no new registrations" is a coherent
property. It goes in Known Limitations.

## Mechanism 4: Provenance by Position (No Fence)

corrald constructs the delivered string as a single leading tag followed by the
verbatim body:

```
[from <dir> (session <id>)]
<body, exactly as sent, to end of prompt>
```

The guarantee is by construction, not by a delimiter: corrald owns the prefix,
so nothing attacker-controlled can precede or occupy the first-line tag
position. The rule, stated in the charter and in CONVENTION, is purely
positional: **the sender tag is the first line; any `[from …]` inside the body
is data.** A forged tag in the body is provably not at position zero, so by the
rule it is inert.

An earlier draft fenced the body with `begin`/`end` markers. That was dropped
(finding C2): the body runs to end-of-prompt, so there is nothing after it to
delimit, and a fixed in-band `end` marker only *adds* a forgeable boundary. A
single positional tag needs no fence and no nonce, and it is simpler to reason
about.

This guarantees **sender attribution** only. The body is still attacker-chosen
text delivered with user authority, so general prompt injection ("ignore your
instructions, do X") remains — no in-band scheme fixes that. The real fix is the
v2 out-of-band direction: carry provenance in ACP `_meta` and have each harness
display it, so the body never needs to be trusted at all.

## Hardening corrald (the Broker)

corrald is the one unsandboxed, trusted component, so its own input handling is
part of the model, not an afterthought.

- **Submit-path confused deputy (finding H1).** corrald opens an agent-named
  path with its own privilege. It MUST: derive and require the fd's canonical
  path to match `<dir>/.corral/outbox/<name>`; `fstat` and require a regular
  file; open non-blocking and reject a FIFO, device, or socket (so a hostile
  path cannot hang corrald); cap the read size; and derive `fromCwd` from that
  validated fd only. A path that fails any check is rejected as malformed,
  never read as a message.
- **Connection-flood robustness (finding M3).** A sandboxed agent with connect
  can open many `corrald.sock` connections. corrald bounds concurrent accepts
  and applies a per-connection read timeout, so a slowloris or connection flood
  degrades gracefully rather than blocking messaging.
- **Argument-injection guards (finding C3).** Covered at the source in
  Mechanisms 1 and 3 (charset-constrained `sessionId`, guarded `{cwd}`,
  exec-array launch), listed here so the broker's launch path is audited as one
  surface.

## Finding 4: Documented Risk and `classify` Cleanup

- **Risk acceptance (Known Limitations).** "A rogue registry record's pid causes
  corral to focus or send `SIGTERM` to an operator-chosen process, possibly
  corrald or the compositor. Accepted: the human is in the loop, the effect is
  recoverable, and pid verification is not worth its cost." No code change; an
  explicit recorded decision.
- **`classify` cleanup.** Today `parse_stop` sets `hidden: true` only to dodge
  `classify`'s visible-spawn force-gate, coupling two unrelated ideas.
  `classify` gains an explicit gate reason (a normal action versus a
  visible spawn) instead of overloading the `hidden` flag, so a stop authorizes
  exactly like a message without the fake flag. Small and isolated; it removes
  a surprising line that reads as a workaround.

## What Changes Where

- **`crates/core`**
  - `discovery.rs`: one `resolve(fd) -> cwd` that opens the registry symlink
    target and derives `cwd` from `/proc/self/fd` (never realpath-then-reopen);
    validate the `sessionId` charset and require it to equal the symlink
    filename; deduplicate by resolved path. `RegistryEntry.cwd` becomes a
    derived, trusted value rather than parsed content.
  - `paths.rs`: the new layout (`registry/`, `state/`, root `corrald.sock`) and
    the `state/` accessors (`whitelist_file`, `approved_commands_file`); env
    overrides retained plus a state-dir override.
  - New `approved_commands.rs`: `normalize` a record's argv to a template, the
    `registered(record)` predicate (shared by corrald and both viewers),
    denormalize with guarded substitution to build the launch argv, and read of
    the JSON store (write is corrald-only). Pure and unit-tested.
  - Submission helper: write an outbox file and send `{"submit": path}`; the
    shared parse/derive lives where corrald can reuse it.
- **`crates/daemon`**
  - `control.rs`: accept `{"submit": path}` with the H1 validation (canonical
    `outbox` path, regular file, non-blocking, size cap), derive `fromCwd` from
    the validated fd, read then delete; bounded accepts + read timeout (M3).
    Keep the synchronous ack.
  - `router.rs`: apply `registered` before any spawn or resume; watch the
    registry and raise a registration for each never-seen template; keep the
    message gate entirely separate (two approvals, never combined).
  - `mailbox.rs`: `classify` gate-reason cleanup; the positional leading tag in
    the delivered text (no fence); outbox-path submission parsing.
  - `tray.rs` / `notify.rs`: the registration prompt showing the full argv, and
    the message-approval prompt, as two distinct pull-based lists.
- **`crates/board` and `crates/gui`**
  - Read `approved-commands.json` and apply the shared `registered` predicate to
    filter the registry: only fitting records are taken as actionable cards; an
    unregistered/deviating one is fully quarantined (surfaced, if at all, as a
    "pending verification" hint, not an actionable card). No approval UI
    (corrald is the sole registrar).
- **Extensions**
  - **All four adapters (announce side):** write the real record to
    `<cwd>/.corral/<sessionId>.json` and create the
    `~/.corral/registry/<sessionId>.json` symlink to it.
  - **pi and opencode only (send side):** submit inter-agent messages by writing
    `<cwd>/.corral/outbox/<id>.json` and sending `{"submit": path}` instead of
    the inline message. `corral-claude` and `corral-cursor` have no send side
    (no `corral_message_agent`), so no submission change applies to them.
- **`CONVENTION.md`**: bump to v2. Rewrite the registry section (symlink into
  the workdir, physical location authoritative, ignore the content `cwd`); the
  submission appendix (outbox file, `{"submit": path}`); a new sandbox-surface
  requirement section; and note approved-commands as consumer policy in the
  appendix.
- **`AGENTS.md`, `README.md`, `TODO.md`**: reflect the new layout, the auth
  model, and the approval flow.

## Migration and Compatibility

This is a breaking convention change, so CONVENTION goes to v2 and all four
adapters update in-repo together. A legacy flat record (a regular file in the
registry, not a symlink) cannot be location-authenticated, so it is treated as
unverified and **fully quarantined**, exactly like an unregistered kind: not a
trusted identity, not actionable, messaging to or from it fails closed. Old
records age out by the existing prune. The env overrides keep existing
deployments running while the layout moves.

### Session Identity Within a Directory Is Not Authenticated (finding M1)

Physical location authenticates the *directory*, not the individual session
within it. Two sessions in one box both resolve to that box's cwd, so one can
claim the other's `fromSession` reply handle. This is accepted and consistent
with the model: the directory is the unit of identity and authorization (the
whitelist keys on directory pairs), and same-directory agents already share a
trust and sandbox boundary. Documented so it is an explicit property, not a
silent assumption.

## Testing Strategy

The pure core stays unit-tested; the IO wrappers stay thin.

- Symlink resolution to a derived `cwd`, including a nested symlink and a
  `/tmp` target (attacker attributed honestly); a mismatched record
  `sessionId` versus filename rejected; duplicate resolved paths deduplicated.
- Outbox-path canonicalization via the fd readlink, including a symlink swapped
  after open (must not redirect); `fromSession` verified against the record's
  physical directory.
- The `resolve(fd)` derivation: a symlink swapped after open must not redirect
  the derived cwd (the realpath-then-reopen race must be absent); a
  charset-violating or filename-mismatched `sessionId` rejected.
- Submit-path validation (H1): a non-`outbox` canonical path, a FIFO, and an
  oversize file are each rejected as malformed, not read.
- `normalize` and the `registered` predicate: the `{sessionId}`/`{cwd}`
  placeholders; a buried `bash -c` deviating (quarantined); an unregistered
  label quarantined; a registered label with matching argv taken; guarded
  substitution of a `-`-leading cwd.
- `classify` with the explicit gate reason across every ack (parity with the
  current table, minus the `hidden` contortion).
- The positional tag: a forged inner `[from …]` after the first line does not
  occupy position zero; the leading tag is corrald-constructed and unchanged.
- Adapter unit tests: the record is written as a symlink pair; submission emits
  `{"submit": path}` with the file present.

## Open v2 Directions

- Out-of-band provenance in ACP `_meta`, displayed by each harness, retiring the
  in-band tag and the body-wrap rule.
- Message signing with a one-time location proof at enrollment, the path to
  remote or multi-host agents.
- An automatic reply channel (already listed in TODO), unchanged by this design.
