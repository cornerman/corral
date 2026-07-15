# The Corral Announce Convention

Convention v1 — 2026-07-13

This document specifies the filesystem-and-socket convention an agent harness
follows to become discoverable, triageable, and drivable by a corral-compatible
consumer. It is written to be implemented from this document alone, without
reading corral's source or the reference `corral-pi` extension.

Two roles:

- An **agent** is one live coding-agent session (e.g. a `pi` process). It
  announces itself and serves a small ACP surface.
- A **consumer** discovers agents, shows which need attention, and drives them
  (focus, resume, prompt). Corral is the reference consumer; it is not the only
  possible one.

The convention rides on ACP (the Agent Client Protocol): the same JSON-RPC an
ACP client speaks to an agent over stdio, carried instead over a per-session
unix socket. Discovery is a per-session file registry, not a network service.

Requirement levels use MUST / SHOULD / MAY as in RFC 2119.

## 1. Model

An agent publishes two things: a **registry record** (how a consumer finds it)
and a **workdir-local socket** (how a consumer talks to it). The registry is the
single discovery store a consumer scans; it could never scan the scattered
working directories directly. The socket lives inside the session's own working
directory, which is the isolation primitive: under a per-session sandbox that
boxes the whole agent to its workdir, only that session (and an unsandboxed
consumer) can reach the socket. Peer authentication relies on directory
permissions alone; there are no ports and no network exposure. A workdir-local
path is used rather than `$XDG_RUNTIME_DIR` precisely because a sandboxed
session cannot reach the latter.

## 2. Registry Record (MUST)

Each live session writes one JSON record naming its socket and the commands a
consumer runs to relaunch it later.

- Path: `$HOME/.corral/registry/<sessionId>.json`. A consumer and agent MAY
  override the directory with `$CORRAL_REGISTRY_DIR`.
- The registry directory MUST be created mode `0700`; the record file SHOULD be
  written mode `0600`.
- The record MUST be written atomically (write a temp file, then rename over
  the target) so a scanning consumer never reads a half-written file.

Fields:

| Field       | Type            | Meaning |
|-------------|-----------------|---------|
| `sessionId` | string          | Stable session identity. MUST match the record's filename and the `sessionId` returned by `session/list`. MUST NOT be the session-file path. |
| `cwd`       | string          | Absolute working directory of the session. |
| `title`     | string \| null  | Human-readable session title; `null` when unnamed. |
| `label`     | string          | Agent kind (e.g. `"pi"`). Appears in the socket filename; a consumer MAY use it to identify a dormant session's kind. |
| `description` | string \| null | Optional. A one-line, human-readable description of the agent kind (e.g. `"pi: terminal TUI coding agent"`), authored by the adapter. A consumer MAY surface it in a capability roster so a caller can pick a kind to spawn; latest-seen per `label` wins. The string is adapter code, not model output. |
| `socket`    | string \| null  | Absolute path to the live socket, or `null` when the session is dormant (cleanly shut down, resumable). |
| `spawnCommand`  | string[] \| null | argv a consumer runs (rooted at a chosen cwd, terminal-wrapped unless `gui`) to start a *fresh* session of this kind, e.g. `["pi"]`. `null` when the agent does not support consumer-launched spawn. |
| `resumeCommand` | string[] \| null | argv a consumer runs to relaunch *this exact* session, e.g. `["pi", "--session", "<sessionId>"]`. `null` when the session is not resumable (ephemeral). A record is dormant/resumable exactly when this is set. |
| `lastSeen`  | string          | ISO-8601 timestamp, refreshed while the session runs. Lets a consumer age out stale dormant records. |
| `gui`       | boolean         | Optional; default `false`. `true` when the agent draws its own window (a GUI app like quine), so the consumer launches `spawnCommand`/`resumeCommand` **directly** rather than wrapping them in a terminal. Absent or `false` means terminal-wrapped, so every existing terminal agent keeps its behavior unchanged. |
| `messageFlag` | string \| null | Optional CLI flag that carries the initial launch message (see §2a), e.g. `"--message"` for quine. When set, the consumer passes the message as this flag's value (`… --message "<text>"`); absent/null means the message is a trailing positional argument. Lets a flag-based agent take a launch message without accepting a positional. |
| `hidden`    | boolean         | Optional; default `false`. `true` when the session runs **hidden**: inside a headless compositor, so its window never maps on the host. A consumer reveals a hidden session by resume (see §2b) rather than by focusing a window, and MAY show it as hidden. The agent SHOULD set this from the `CORRAL_HIDDEN=1` environment variable a consumer exports when it launches a hidden session (see §2b). Absent/false is a normal, visible session. |

The consumer runs `spawnCommand` / `resumeCommand` **verbatim and never parses
them**, so it stays agent-neutral: pi's `--session` grammar, opencode's, and any
other kind's live in the agent, not the consumer. For a terminal agent (`gui`
absent/false) the consumer wraps the argv in its own terminal
(`<terminal> -e <argv…>`) rooted at `cwd`; for a GUI agent (`gui: true`) it runs
the argv directly (the app opens its own window). Either way the consumer MAY
append an initial user message as a trailing positional argument (see §2a). A resume
command SHOULD launch in the record's `cwd`, so an agent MAY address the session
by a short id (resolved per-project) rather than an absolute path.

Example:

```json
{
  "sessionId": "6f1c2e7a-3b4d-4c5e-9a10-2f8b1d0e4c33",
  "cwd": "/home/dev/projects/widget",
  "title": "fix the flaky retry test",
  "label": "pi",
  "socket": "/home/dev/projects/widget/.corral/pi-48213.sock",
  "spawnCommand": ["pi"],
  "resumeCommand": ["pi", "--session", "6f1c2e7a-3b4d-4c5e-9a10-2f8b1d0e4c33"],
  "lastSeen": "2026-07-13T09:41:07.512Z"
}
```

(A GUI agent adds `"gui": true`; a consumer then launches its commands without a
terminal wrapper.)

```text
(illustrative GUI record fields: "label": "quine", "gui": true,
 "messageFlag": "--message",
 "spawnCommand": ["quine", "--corral"],
 "resumeCommand": ["quine", "--session", "<sessionId>", "--corral"])
```

A record with `socket == null` denotes a **dormant** session: not running, but
resumable via `resumeCommand`.

### 2a. Launch (initial message injection)

When a consumer launches a session (`spawnCommand` or `resumeCommand`) to
deliver a message, it appends the message to the argv. By default it is the
final positional argument, and as a generic CLI-safety convention the consumer
space-guards a message that starts with `-` or `@` (prefixes one space) so an
arg parser does not read it as a flag or a file. If the record sets
`messageFlag`, the consumer instead passes the message as that flag's value
(`… <messageFlag> "<text>"`), bound to the flag, so no guard is needed. Either
way an agent that accepts an initial message gets atomic launch-with-delivery;
one that does not simply ignores it.

### 2b. Hidden launch (background session)

A consumer MAY launch a session **hidden**, so it runs and announces normally
but never maps a window on the host. It does this by running the same
`spawnCommand`/`resumeCommand` inside a headless compositor and exporting
`CORRAL_HIDDEN=1` into the launched environment. The reference consumer wraps
the argv as `env WLR_BACKENDS=headless CORRAL_HIDDEN=1 cage -- <argv…>`
(`cage` is a single-app headless compositor; `WLR_BACKENDS=headless` keeps it
off the host display server, and its bundled XWayland hosts X11 agents). The
wrapping is a consumer detail; the contract on the agent is only: **if
`CORRAL_HIDDEN=1` is set, write `"hidden": true` in the record** (see §2).

Because a live window cannot migrate between compositors, revealing a hidden
session is a **resume**, not a live move: the consumer kills the running
instance (the record then goes dormant) and relaunches it visibly via
`resumeCommand`. Hiding a visible session is the mirror (close its window, then
relaunch hidden). An agent needs no code for reveal/hide beyond the normal
clean-shutdown and resume behavior it already implements.

## 3. Workdir-Local Socket (MUST)

- Path: `<cwd>/.corral/<label>-<pid>.sock`. An agent MAY override the directory
  with `$CORRAL_SOCKET_DIR`.
- The directory MUST be created mode `0700`.
- The filename MUST encode the agent `label` and process id as
  `<label>-<pid>.sock`, so a consumer can read both from the path.
- The socket MUST speak newline-delimited JSON-RPC 2.0: one JSON object per
  line, `\n`-terminated, in both directions. This is ACP exactly as spoken over
  stdio, framed by newlines on the stream.
- The socket SHOULD accept multiple concurrent client connections and answer
  each from current state.

## 4. ACP Request Surface

Requests a consumer sends; the agent replies on the same connection.

### `initialize` (MUST)

Reply with the agent's identity and capabilities:

```json
{
  "protocolVersion": 1,
  "agentCapabilities": { "loadSession": false },
  "agentInfo": { "name": "pi", "version": "0.80.3" },
  "authMethods": []
}
```

`agentInfo.name` is the agent kind (SHOULD match the record `label`).

### `session/list` (MUST)

Reply with this session's identity:

```json
{ "sessions": [ { "sessionId": "6f1c2e7a-…", "title": "fix the flaky retry test", "cwd": "/home/dev/projects/widget" } ] }
```

`sessionId` MUST equal the registry id (§2), so a consumer can match a live
socket to its record and route a session-addressed message to the right agent.

### `session/prompt` (SHOULD)

Inject a user message into the session. Params carry `prompt` as an array of
content blocks; an agent MUST read at least `{ "type": "text", "text": … }`
blocks. Respond with `{ "stopReason": … }` once the injected message has been
consumed by a turn. Whether the message runs immediately or is queued as a
follow-up while the agent is busy is implementation latitude; a busy agent
SHOULD queue rather than reject. Serving `session/prompt` is what makes a
consumer able to deliver messages to the session; an agent that omits it is
discoverable and triageable but not messageable.

### `session/cancel` (SHOULD)

A notification (no `id`). The agent SHOULD abort the current turn.

Any other method SHOULD be answered with JSON-RPC error `-32601` (method not
found).

## 5. Broadcasts (`session/update` notifications)

The agent pushes unsolicited notifications to every connected client. Each rides
one envelope:

```json
{ "jsonrpc": "2.0", "method": "session/update",
  "params": { "sessionId": "6f1c2e7a-…", "update": { … } } }
```

The `update` object's `sessionUpdate` field names the variant.

### `state_update` (MUST)

```json
{ "sessionUpdate": "state_update", "state": "running" }
```

`state` is one of `running`, `idle`, `requires_action` (the ACP v2
prompt-lifecycle vocabulary, see §7). The agent MUST emit it on turn start
(`running`) and turn end (`idle`), and MUST use `requires_action` while the
session is blocked waiting on user input. A newly connected client MUST be
seeded with the current `state_update` immediately on connect, so a consumer can
column the session without waiting for the next transition.

### `session_info_update` (SHOULD)

```json
{ "sessionUpdate": "session_info_update", "title": "fix the flaky retry test" }
```

Emitted whenever the title changes, so an already-connected client sees a rename
(or a first-message fallback title) without reconnecting.

### Activity events (MAY)

For richer board cards, an agent MAY broadcast message and tool activity:

- `user_message_chunk` / `agent_message_chunk`, each with
  `content: { "type": "text", "text": … }`.
- `tool_call` with `toolCallId`, `title`, `status`, and optional `rawInput`.
- `tool_call_update` with `toolCallId` and `status`.

## 6. Lifecycle

- **Start.** Write the registry record with `socket` set, and bind the socket.
- **Running.** Refresh `lastSeen` periodically (e.g. at each turn end) and
  rewrite `title` on rename, keeping the record current.
- **Clean shutdown.** Unlink the socket and rewrite the record with
  `socket: null`, leaving a dormant, resumable entry. Do this before removing
  the socket file so a consumer never sees a record pointing at a vanished
  socket during normal shutdown.
- **Crash.** A session that dies without a clean shutdown leaves its record with
  a stale, non-null `socket`. Detecting this is the consumer's job: a consumer
  that fails to connect to a record's socket SHOULD treat that record as
  dormant (still resumable) rather than delete it, and a freshly starting
  socket that has not yet been proven dead SHOULD stay on the live path so it
  does not flicker through the dormant view.
- **Pruning.** Forgetting dormant records is consumer policy and is not
  specified here. A consumer SHOULD prune conservatively — e.g. only past a
  staleness horizon measured from `lastSeen` — and MUST NOT delete a record it
  cannot parse or does not understand (a schema change or a newer producer must
  never destroy history); such a record is ignored, not removed.

## 7. ACP Conformance

The request surface (`initialize`, `session/list`, `session/prompt`,
`session/cancel`) and the message/tool activity events are ACP v1.

`state_update` follows the ACP v2 Prompt Lifecycle RFD
(agentclientprotocol.com/rfds/v2/prompt), which adds a session/update carrying
`running` / `idle` / `requires_action`, broadcast to every connected client
rather than only the prompt sender. This convention emits that exact shape and
vocabulary ahead of stabilization, so there is zero migration when ACP v2 lands.
The tradeoff: a strict v1-only client that rejects unknown `sessionUpdate`
variants will not triage a session until it understands `state_update`.
Implementations SHOULD ignore `sessionUpdate` variants they do not recognize.

`protocolVersion` in the `initialize` reply is `1` today; it tracks the ACP
protocol version, not this convention's version.

## 8. Conformance Checklist

An agent is a first-class board citizen when it satisfies the MUST items; the
SHOULD and MAY items add messaging and richer cards.

MUST:

- [ ] Write `$HOME/.corral/registry/<sessionId>.json` (dir `0700`, atomic write)
      with all fields of §2, `sessionId` matching the filename.
- [ ] Bind `<cwd>/.corral/<label>-<pid>.sock` (dir `0700`) speaking
      newline-delimited JSON-RPC 2.0.
- [ ] Answer `initialize` with `agentInfo`.
- [ ] Answer `session/list` with `sessionId` equal to the registry id.
- [ ] Broadcast `state_update` (running/idle/requires_action) on transitions and
      seed it to each new client on connect.
- [ ] On clean shutdown, unlink the socket and set the record's `socket` to
      `null`.

SHOULD:

- [ ] Serve `session/prompt` (makes the session messageable) and `session/cancel`.
- [ ] Broadcast `session_info_update` on rename.
- [ ] Refresh `lastSeen` while running.

MAY:

- [ ] Broadcast `user_message_chunk` / `agent_message_chunk` / `tool_call` /
      `tool_call_update` for activity lines.

## Appendix A — Agent-Initiated Messaging (optional, non-normative)

This appendix is not part of the core convention. It describes how an agent
*sends* a message to another session, which depends on corral (or an equivalent)
acting as the trusted cross-workdir router. A harness MAY implement it to let its
agents initiate cross-session messages; omitting it costs only the send
direction (the agent is still fully discoverable, triageable, and messageable
*by* the consumer).

Sandboxed agents cannot reach each other's sockets, so an agent does not deliver
directly. It submits one message per connection over the consumer's control
socket, newline-delimited JSON (request line, then one ack line):

- Socket: `$HOME/.corral/corrald.sock` (override `$CORRAL_CONTROL_SOCKET`). A
  connect failure means the consumer is not running, so submission fails loud
  rather than queuing silently.

Request fields:

| Field           | Type              | Meaning |
|-----------------|-------------------|---------|
| `id`            | string            | Unique message id. |
| `fromCwd`       | string            | Sender's working directory (the routing authorization is keyed on directory pairs). |
| `fromSession`   | string            | Sender's `sessionId`, a reply handle so the receiver can answer this exact agent. |
| `message`       | string            | The message text. |
| `targetDir`     | string (one of)   | Deliver to whoever works in this directory (spawning one if none). |
| `targetSession` | string (one of)   | Deliver to this exact session id (resuming it if dormant). Exactly one of `targetDir` / `targetSession` is set. |
| `forceNew`      | boolean           | With `targetDir`: spawn a dedicated fresh agent instead of reusing one. |
| `label`         | string (optional) | With `targetDir`: which agent kind to spawn (matched against a record's `label`). Omitted falls back to the directory's own kind; an unknown label fails loud. |
| `hidden`        | boolean (optional)| Whether a spawn/resume this message triggers runs hidden (no window). Defaults `true`, so an uninvited agent never pops a window. `false` requests a visible window and always requires operator approval (a visible window is a stronger action than a message, so the whitelist alone never authorizes it). Ignored when the target is already live. |
| `createdAt`     | string            | ISO-8601 creation time. |

Ack (one line, `{"status":"…"}`), computed synchronously from the registry and
whitelist:

| `status`              | Meaning |
|-----------------------|---------|
| `accepted`            | Recipient found and the `(sender -> target)` pair is authorized; will route. |
| `approval_needed`     | Recipient found but not yet authorized; held for the operator's approval (not awaited). |
| `recipient_not_found` | `targetSession` is not in the registry. |
| `directory_not_known` | `targetDir` is not an existing directory. |
| `malformed`           | Unparseable request. |

The consumer authorizes the `(fromCwd -> target directory)` pair, resolves the
target (reusing, spawning, or resuming as needed), and injects the message with
a provenance tag naming the sender directory and session. The receiver replies
by addressing `targetSession` = the sender's `fromSession`. The ack confirms
receipt and resolution, not delivery; an `approval_needed` message is delivered
after approval without a further ack.

### Roster query (`list`)

Over the same control socket, an agent MAY send a read-only roster query
(`{"op":"list","fromCwd":"<dir>"}`) and read one reply line
(`{"status":"ok","agents":[…]}`). It is ungated: it exposes only what the caller
could already reach. Each agent entry is one of two shapes:

- **Visible** (the caller's own directory, or a whitelisted `(fromCwd -> dir)`
  pair): `kind`, `description`, `cwd`, `sessionId`, `live`, `canMessage: true`.
  The `sessionId` is a `targetSession` the caller may message.
- **Anonymous** (every directory the caller may not reach, folded by kind):
  `kind`, `description`, `canMessage: false`, no `cwd` / `sessionId`. So a
  caller learns which agent kinds exist without learning who runs where.

A roster entry never carries a session's `title` or activity: messaging is not
reading. A charter prepended to a freshly spawned agent's first prompt teaches
it these two verbs (message + list) and the swarm discipline (confirm the task,
escalate uncertainty up, stay event-driven).

## Appendix B — Reference Implementation (non-normative)

- Agent side, first example: `extensions/corral-pi.ts` (a pi extension
  implementing all of §2–§6 and Appendix A).
- Agent side, second example: `extensions/corral-opencode.ts` (an opencode
  plugin implementing the same §2–§6 surface and Appendix A). It is the
  cross-harness proof: a different harness joins the board with no change to a
  consumer, since a consumer runs the record's launch commands verbatim and
  reads its `label` generically.
- Consumer side: `crates/board/src/discovery.rs` (registry scan and record
  parsing) and `crates/board/src/watch.rs` (socket connect, seed, and broadcast
  handling).
