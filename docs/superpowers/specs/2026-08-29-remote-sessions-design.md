# Remote Sessions: Agents On Another Machine Join The Board

Status: design, 2026-08-29. Input: `docs/research/2026-08-26-remote-and-mobile-agent-access-survey.md`.

## Problem

corral sees exactly one machine. Discovery, identity and delivery all rest on a
shared filesystem: a record physically at `<cwd>/.corral/registry/<id>.json`
proves the agent works in `<cwd>` (CONVENTION §1, SECURITY.md), the board opens
each agent's workdir-local socket directly, and corrald routes over unix
sockets. An agent running on a second host cannot appear, cannot be messaged,
and cannot message in.

The wanted capability, stated by the maintainer: run a harness on another
machine, see it on the local board, and talk to it. Both machines already run
corrald today. A phone or thin client that runs a harness without corrald is a
later chapter, and this design must not foreclose it.

VISION.md has carried the open question since 2026-07-26 ("remote harnesses need
a network transport, and the open question is how far it stays outside corral's
own process"). SECURITY.md declares multi-host in scope and under design. This
document answers both.

## Decisions

Each decision below was taken deliberately, with the alternatives that lost.

### D1. A Separate Binary Owns The Network

`corral-link`, one process per machine, its own systemd user unit. It is the
only component that speaks to another host.

corrald today depends on `serde_json` and `ksni` only, is synchronous, and is
the process TODO.md ranks as the highest-value hardening target ("unsandboxed,
full-authority RCE surface"). Moving tokio, quinn and rustls plus an
internet-reachable endpoint into it would enlarge exactly the surface the
project wants to shrink. A separate process also gives the sandbox story a seam:
`corral-link` can be confined independently of the broker.

Rejected: transport inside corrald (fewest parts, wrong process); a `corrald
link` subcommand as a second process (keeps the process boundary, loses the
dependency boundary); external plumbing plus glue scripts (no live state, no
identity, unowned shell).

Consequence for `t13_no_tcp_listener_in_workspace`: the claim narrows from "no
crate names a TCP type" to "no local component does". The guard keeps scanning
`crates/`, and `crates/link` is excluded with a comment naming this document.

### D2. The Protocol Is Symmetric, The Deployment Need Not Be

A link endpoint publishes 0..N sessions and consumes 0..N. A machine running
corrald publishes many. A future Android harness publishes one and runs no
corrald. A phone viewer publishes none. One protocol serves all three, so the
thin-client chapter needs no second dialect.

Which side dials is a NAT and configuration detail, not a role. Either side may
dial; the connection is bidirectional once established.

Rejected: one-way publication (a remote box could then never run a board over
the link); explicit hub and spoke roles (a role concept and a single point of
failure, for a machine count that does not need it).

### D3. Transport Is iroh, Behind A Seam

`iroh` 1.0 (released 2026-06-15, wire and API stable, MIT/Apache) dials an
ed25519 public key, punches NAT holes, falls back to a relay, and encrypts end
to end, so a relay carries ciphertext only. Swift and Kotlin bindings exist
(`n0-computer/iroh-ffi`), which is the mobile door this design must leave open.

QUIC pays a second dividend beyond reachability: stream multiplexing. One
control stream plus one bidirectional stream per proxied session means corral
invents no framing layer for a multiplexed link.

The transport sits behind a trait so a plain unix-socket transport (bring your
own `ssh -R` or Tailscale tunnel) can be added later without touching link
semantics. It is not implemented in v1.

Relay and discovery policy: no pkarr and no DNS publishing, so nothing about
these machines is globally discoverable. A peer is addressed by (node id, relay
URL, optional direct addresses) read from the peers file. The relay defaults to
n0's free public relays and is overridable to a self-hosted `iroh-relay`. Note
that n0's *managed* relays became token-authenticated in July 2026; the free
public fallback relays remain open.

Rejected: unix-socket transport first (cheaper and it proves the semantics, but
the maintainer chose to debug QUIC now rather than build tunnel setup per pair);
Tailscale (hard dependency on a tailnet, one Android VPN slot); custom mTLS
(corral would own certificate issuance, rotation and revocation).

### D4. Enrollment Is An Operator File

- Own identity: `~/.corral/link/secret.key`, mode 0600, generated on first run.
- `corral-link id` prints this machine's node id and relay URL for out-of-band
  exchange.
- `corral-link enroll <name> <nodeId> [--relay <url>] [--addr <ip:port>]`
  writes `~/.corral/peers/<name>.json`.
- An inbound connection from an unenrolled node id is refused at accept.

The peers file is operator-owned and never agent-writable, the same discipline
the whitelist and `approved-commands.json` already follow. `<name>` is local
only: machine A may call B `desktop` while B calls A `laptop`. A name is
therefore always read as "as seen from here".

Rejected: a pairing code ceremony (nicer for a phone, but it obliges the daemon
to process unsolicited inbound handshakes); borrowing the transport's identity
(no key store, but then nothing meaningful can be shown to the operator at
approval time).

### D5. Namespace: The Peer Name Is The Host Component

- A card shows `desktop:/srv/x` where it shows `/srv/x` today.
- A whitelist line reads `desktop:/srv/x -> /home/me/proj`. The local side stays
  unprefixed, so every existing line keeps its meaning and no migration runs.
- A remote session is addressed as `<peer>:<sessionId>` in `target_session` and
  in the `corral_list_agents` roster. Local ids stay bare.
- The provenance tag grows the same prefix: `[from desktop:/srv/x (session
  desktop:abc123)]`, so a reply handle stays precise across hosts.

### D6. A Remote Agent Gets A Local Proxy Socket

The consuming machine's `corral-link` binds one unix socket per remote session
and tunnels ACP verbatim to the publishing machine, which relays it into that
agent's own socket. The board then does exactly what it does today: connect,
stream `state_update`, tool activity, title, model and context, and send
prompts, cancels and `session/load` back.

This is the decision that keeps the board simple. Live state, operator `m`,
card-move cancel and nudge, and history export all work with no second render
path and no record schema growth beyond a `host` marker.

Rejected: mirroring card content into the record and rendering from it (less
traffic and no proxy, but a second render path in both shells, prompts rerouted
through corrald, and no history export).

### D7. The Link Meets corrald In `$XDG_RUNTIME_DIR`

Mirrored records, proxy sockets, and the link's control channel to corrald live
under `$XDG_RUNTIME_DIR/corral/`, not under `~/.corral/`.

This is load-bearing, not cosmetic. `~/.corral` is on the agent sandbox
allowlist (that is why an agent can reach `corrald.sock` at all), so a local
agent could otherwise write a forged "remote" record or a forged remote
submission and impersonate a peer host. `$XDG_RUNTIME_DIR` is precisely what a
sandboxed agent cannot reach, which is why CONVENTION §1 rejected it for
announce. Here that property is the asset: location-is-identity keeps
authenticating `corral-link` itself, and only the *host* component of a mirrored
record comes from the authenticated peer instead of from the filesystem.

Layout:

```
$XDG_RUNTIME_DIR/corral/link.sock                       link -> corrald control channel
$XDG_RUNTIME_DIR/corral/remote/<peer>/registry/<id>.json  mirrored record (link-written)
$XDG_RUNTIME_DIR/corral/remote/<peer>/<id>.sock           ACP proxy socket (link-bound)
~/.corral/link/secret.key                                this machine's identity (0600)
~/.corral/peers/<name>.json                              enrolled peer (operator-owned)
```

corrald curates `remote/<peer>/registry/` as a second input with a different
identity rule: cwd comes from the peer's claim, host comes from the directory
name, everything else is validated as today. Survivors land in the sealed
`state/registry/` with `host` set and `focusable` false. The boards keep reading
exactly one directory.

### D8. Everything But Focus, Hidden By Default

| Action | Remote behaviour |
|---|---|
| `m` message | works, over the proxy socket |
| `o` history | works, `session/load` over the proxy socket |
| card move: cancel, nudge | works, over the proxy socket |
| `d` stop | forwarded op, the peer's corrald kills the process |
| Enter on a dormant card | forwarded op, resumes there, hidden |
| Shift+Enter spawn | forwarded op, spawns there, hidden |
| Enter on a live card | refused with a status: no window on this machine |
| `h` toggle hidden | works, forwarded op |

Stop, spawn and resume cannot act on a remote pid, so they ride the existing
submission envelope, forwarded to the peer's corrald and executed there. Remote
launches start hidden because nobody is sitting at that screen; `h` still
toggles afterwards.

Focus is the only capability with no remote meaning, and the board says so the
way it already reports that history is unavailable on a dormant card.

### D9. Trust Split Carried Across Unchanged

- **Operator actions over an enrolled link are ungated.** Enrolling a peer means
  trusting that machine's operator. Gating them would mean asking the operator
  to approve the operator, which is the exact argument that makes local `m`
  direct.
- **Agent-initiated messages stay gated** on the `(host:dir -> host:dir)` pair,
  with the same tray and notification approval and the same "allow always".
- Enrollment therefore becomes the one heavy decision in the system, and
  SECURITY.md must say so in those words.

The gate runs on the *receiving* machine, since that is the machine whose
operator owns the target agent.

Rejected: approval for every cross-host operator action (Claude Code's
`isolatePeerMachines`; safer if a peer key is stolen, but it asks you to approve
your own actions on your own second machine); a per-peer `trustOperator` flag
(one more knob and one more state in the threat model, for a case nobody has).

CONFIRM WITH THE MAINTAINER: this decision was taken as the recommendation
after the question went unanswered.

### D10. A Down Link Marks Cards, It Does Not Hide Them

Remote cards stay visible with an explicit `link down` marker and a stale-since
age, all actions disabled, pruned after a TTL (15 minutes, one constant). An
attention board must say "that machine went away" rather than quietly lose the
agent that was waiting for you. The marker is honest about staleness, so the
"never fake agent state" rule holds.

Rejected: immediate disappearance (simplest, but an agent in Requires Action
vanishes on a wifi hiccup); demotion to the Dormant column (dormant means
"cleanly shut down, resume me", not "unreachable").

### D11. Anti-Abuse Lands Local First, Then On The Link

The link ships behind a preparatory phase that closes the local caps already
queued in TODO.md, because a network peer makes each of them cheaper to
exercise:

1. Cap pending approvals per sender directory; coalesce the desktop surface into
   one notification naming a count (TODO #1, SECURITY.md T21).
2. Cap pending registrations per label; rate-limit the audit line (TODO #2).
3. Bound the pointer-store scan per tick (TODO #3).
4. Refuse control characters in whitelist paths (TODO #4, T5).
5. Write the history export 0600 (TODO #5).

The link then adds its own, enforced in `corral-link` where untrusted bytes
arrive: a per-message size cap, a per-peer rate cap, a bounded inbound queue
that drops with one audit line when full, and duplicate suppression over a short
window.

## Architecture

```
machine A (laptop)                                machine B (desktop)
  corrald  -- state/registry (vetted) -->  corral-link  ==iroh QUIC==  corral-link
     ^                                                                     |
     | curates remote/<peer>/registry                                      | reads B's
     |                                                                     | state/registry
  corral / corral-gui                                                      | + agent sockets
     |  connects proxy socket  -----------> $XDG_RUNTIME_DIR/corral/remote/desktop/<id>.sock
```

Streams on one connection:

- **control stream** (one, bidirectional): session announce, update and removal
  for every session the endpoint publishes, plus forwarded ops (`spawn`, `stop`,
  `message`, `list`) in the existing submission envelope, plus liveness pings.
- **session streams** (one bidirectional per proxied session): raw ACP JSON-RPC,
  opened by the consumer when a board first connects to the proxy socket, closed
  when the last board disconnects.

Every frame stays JSON-RPC shaped, so ACP's eventual Streamable HTTP transport
can replace this framing without touching the semantics (research doc, Protocol
Landscape).

## Crates And Files

- `crates/link` (new, binary `corral-link`): `main.rs` (loop and units),
  `peers.rs` (enrollment store, pure), `transport.rs` (trait plus the iroh
  implementation), `publish.rs` (own `state/registry` to the wire), `mirror.rs`
  (wire to `remote/<peer>/`, proxy socket lifecycle), `limits.rs` (D11 caps,
  pure). Only this crate depends on `iroh`.
- `crates/core`: `paths.rs` gains the `$XDG_RUNTIME_DIR/corral/` locations;
  `model.rs` gains `Agent.host` plus the `LinkDown` marker and the focus
  refusal; `discovery.rs` parses `host` on a record.
- `crates/daemon`: `curator.rs` curates the second input directory;
  `mailbox.rs` parses and matches host-qualified whitelist pairs and roster
  entries; `router.rs` forwards an op whose target carries a host.
- `crates/board`, `crates/gui`: host badge on the card, `link down` state,
  focus refusal status. Both shells, per the parity rule.
- `nix/hm-module.nix`: `programs.corral.link` options (enable, relay URL).

## Testing

`checks.e2e-remote`, a two-node NixOS VM test (`nix/tests/scenarios/remote.py`)
with nodes `alpha` and `beta`, both running corrald, corral-link and the stub
LLM. Relays are disabled and direct addresses are pinned in the peers files, so
the test needs no internet and no relay.

Assertions, in order: enrollment refuses an unenrolled node id; a pi session
started on `beta` appears in `alpha`'s `state/registry` with `host` set and is
readable through the proxy socket; `alpha`'s operator message reaches it and the
transcript shows it; an agent on `alpha` messaging an agent on `beta` is refused
until the host-qualified whitelist line exists, then delivered with the
host-qualified provenance tag; a remote spawn maps no window on either machine;
a remote stop leaves a dormant, resumable record on `beta`; killing `beta`'s
link marks `alpha`'s cards `link down` and disables their actions.

Unit tests carry the pure parts as usual: peers parsing, host-qualified
whitelist matching, the caps in `limits.rs`, and record mirroring.

## Documentation Duties

- `AGENTS.md`: a Remote Sessions section, the new crate in Crates, the new
  interface in Interfaces To The Outside World, and the e2e scenario in the hard
  rule.
- `SECURITY.md`: replace the "multi-host under design" paragraph with the real
  boundary. New threats to number: forged remote record by a local agent (D7
  answers it), stolen peer key equals full operator authority (D9 accepts it),
  relay metadata exposure (accepted), link flood (D11).
- `CONVENTION.md`: a new section for the link protocol, or a sibling `LINK.md`.
  The announce convention itself does not change.
- `VISION.md`: close the open question with D1.
- `README.md`: one line in the key table only.

## Non-Goals For v1

- No phone or thin-client implementation. The protocol admits one; nothing
  ships.
- No pairing ceremony. Enrollment stays a file.
- No per-peer export filtering. An enrolled peer sees every session of this
  machine, which follows from D9.
- No transitive links. A peer's peers are not visible here.
- No unix-socket transport implementation, though the seam exists.
- No focus across a link, ever.

## Open Questions

1. **D9 confirmation.** Ungated operator actions over an enrolled link was
   chosen without an explicit answer.
2. **Proxy socket lifetime.** Open a session stream per connected board, or one
   shared stream fanned out by the link? Shared is less traffic; per-board is
   simpler and matches how agents already serve several clients. Decide during
   implementation, measured against the number of boards actually run.
3. **Peer name collisions with a local directory prefix.** A local path
   containing `:` would be ambiguous in a whitelist line. Refuse `:` in a peer
   name and in a whitelisted local path, the same way `" -> "` is refused today.
