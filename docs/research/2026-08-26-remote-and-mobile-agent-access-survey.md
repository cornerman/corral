# Remote and Mobile Agent Access: Field Survey

Status: research (2026-08-26). Input for the remote-harness design, which is now
in scope (see SECURITY.md, VISION.md). No decisions here, only the field and the
constraints it imposes.

## Headline

The field has converged on one shape, and Anthropic has already shipped corral's
feature set inside Claude Code. Every product that reaches an agent from
somewhere else uses **outbound-only connections from the machine that runs the
agent** to a rendezvous both sides can reach, with identity from an account or a
key rather than from the filesystem. Nobody opens an inbound port on a laptop,
because laptops and phones have no address.

Claude Code now ships: per-session unix-socket inboxes with `ListAgents` /
`SendMessage` (corral's `corral_list_agents` / `corral_message_agent`),
cross-machine delivery relayed through Anthropic servers, per-session inbound
controls (`accept` / `hold` / `refuse`), an `isolatePeerMachines` switch that
demands operator approval before a message leaves the machine, a board of
running sessions (agent view), and Channels, which push events from Telegram,
Discord or iMessage into a live session. Read
[cross-session-messaging](https://code.claude.com/docs/en/cross-session-messaging),
[remote-control](https://code.claude.com/docs/en/remote-control) and
[channels](https://code.claude.com/docs/en/channels).

What remains corral's alone: harness neutrality (Claude's version reaches only
Claude sessions), self-hosting with no vendor relay holding the transcript, and
composition over ownership. What corral must now learn from a competitor with
more usage: the anti-abuse rules, the per-session inbound control, and the
cross-machine approval switch.

## Who Does What

| System | Where the agent runs | Transport | Identity / trust | Open source |
|---|---|---|---|---|
| Claude Code Remote Control | your machine | outbound HTTPS to Anthropic, poll + stream | claude.ai account, short-lived scoped credentials, optional device enrollment plus biometric step-up | no |
| Claude Code cross-session messaging | your machines | unix socket same-host, Anthropic relay cross-host | OS user same-host; account cross-host | no |
| Claude Code Channels | your machine | MCP server pushing into the session | per-channel sender allowlist, bootstrapped by a pairing code | plugins yes |
| [Happy](https://github.com/slopus/happy) | your machine | wrapper CLI to a relay server, end-to-end encrypted | device keys, relay sees ciphertext | MIT |
| [Omnara](https://github.com/omnara-ai/omnara) | your machines | `omnarad` per machine, plus api/worker/Postgres/Redis | account and org roles | Apache-2.0 |
| ClaudeCodeUI | your machine | web server | account | GPL |
| Cursor mobile, Terragon | vendor cloud VM | vendor | account | no |
| SSH plus tmux, mosh | your machine | direct SSH | ssh keys | yes |

Two structural facts stand out. Omnara's `omnarad` is a **per-machine daemon on
every host agents run on**, the same shape as corral's `corrald` in spoke mode.
Happy's split is **wrapper on the host, relay in the middle, viewer on the
phone**, with the relay holding only ciphertext, which is the strongest privacy
posture in the table and the one closest to corral's values.

## Patterns Worth Stealing

- **Outbound only, no inbound listener.** Claude Code states it plainly: the
  local session makes outbound HTTPS requests and never opens a port. This is
  the same conclusion as corral's hub-and-spoke choice, reached from NAT rather
  than from security.
- **Local stays local.** Claude relays only when the peer is on another machine;
  same-host delivery goes over the per-session socket and never leaves. corral
  should keep the same rule, so the remote path is an addition rather than a
  replacement.
- **Per-session inbound control.** `crossSessionInbound` = `accept` / `hold` /
  `refuse`, with held messages surfaced for approval and released if the setting
  later allows them. corral gates on the `(sender-dir, target-dir)` pair only;
  the receiving session has no say. A remote sender makes that gap wider.
- **A separate switch for leaving the machine.** `isolatePeerMachines` forces
  operator approval for any message crossing hosts, even under
  `bypassPermissions`. The direct analogue: a remote pair is never whitelisted
  implicitly, and possibly never auto-approved at all.
- **Anti-abuse that corral entirely lacks.** Size cap (about one million
  characters), per-sender burst refusal at the sender, duplicate suppression in
  a short window, a queue cap of 50, and a documented guarantee that a message
  loop between two sessions stops by itself. Two agents on a network link can
  loop far more expensively than two on one host.
- **Reply address as a capability.** A cross-machine message that has no return
  path arrives *without* a reply address, and the sender is told so. corral's
  provenance tag is the same idea; remote delivery needs the same honesty when
  the return path is down.
- **One-shot idle subscription.** `notify_when_idle` asks a peer to report once
  when it next goes idle, with a 12-hour expiry and no polling on either side. A
  cheap alternative to a remote board watching state continuously over a link.
- **Pairing codes and device enrollment.** Channels bootstrap their allowlist by
  a code exchanged in the chat itself; Remote Control can require an enrolled
  device plus a sign-in less than 18 hours old. Both are enrollment ceremonies,
  which is exactly what corral needs to replace "the file is here, so I know who
  wrote it".
- **Push notification as the phone's attention channel.** The board's job on a
  phone is mostly "tell me when you need me", which is a push notification, not
  a rendered board.

## Transports, Against corral's Constraints

corral is Rust, self-hosted, sandbox-friendly, and currently proves in a test
(`t13_no_tcp_listener_in_workspace`) that no shipped crate even names a TCP
type. Options, cheapest first:

1. **Bridge a unix socket with something else.** OpenSSH forwards unix sockets
   in both directions (`ssh -R remote_socket:local_socket`, verified in
   OpenSSH 10.4 on this machine), and Tailscale or a plain `socat` can do the
   same. corral keeps speaking to a socket path and never creates a network
   connection, so the T13 guard survives verbatim and there is no new protocol,
   no PKI, and no reconnect logic in corral. The cost is per-pair setup outside
   corral and an awkward story on Android.
2. **Tailscale.** WireGuard mesh, stable addresses, NAT traversal, and identity
   from the local `tailscaled` LocalAPI `whois` call, which turns a source IP
   into a node and user without corral holding any certificates. The Android
   app captures Termux traffic, and Termux plus Tailscale is a documented way to
   reach a phone. The cost is a hard dependency on a tailnet and one Android VPN
   slot.
3. **[iroh](https://github.com/n0-computer/iroh).** A Rust library for QUIC
   connections dialed by public key, with hole punching and relay fallback. No
   VPN, identity is the node key, and it embeds in the daemon. The cost is a
   substantial dependency and a relay to run or borrow when hole punching fails.
4. **TLS or WebSocket with mTLS or a pre-shared token.** Full control, no
   dependency, and corral owns certificate issuance, rotation and revocation.
   That last part is the real cost.
5. **A broker (NATS leaf nodes, MQTT).** Built for many reconnecting spokes and
   subject-based routing. The cost is a broker to operate and a second
   authorization model beside the whitelist.
6. **Push wake (UnifiedPush / ntfy, or FCM).** Only relevant if a phone whose
   process the OS killed must be revived from outside. Orthogonal to the
   transport choice.

## Android Reality

Every practical Android coding agent runs in a Linux userland: Termux
([opencode-termux](https://github.com/guysoft/opencode-termux),
[claude-code-android](https://github.com/ferrumclaudepilgrim/claude-code-android))
or the Android 16 Linux Terminal, a Debian VM on the Android Virtualization
Framework. The convention's assumptions (filesystem, unix sockets, own-uid
`/proc`) therefore hold unchanged, and no filesystem-free announce path is
needed for a native app that nobody ships.

The binding constraint is process lifetime, not connectivity. Android 15 kills
Termux background processes even with `termux-wake-lock` and unrestricted
battery ([termux-app#5150](https://github.com/termux/termux-app/issues/5150)),
so a phone spoke dies and returns constantly. Connectivity is solved
independently: with Tailscale the phone has a stable address and can even run
`sshd`.

Everything else in the field treats the phone as a **viewer**, not a host:
Claude's mobile app, Happy's app, Omnara's app, and Channels, which uses an
existing chat app as the client so there is no app to build at all. That splits
the work into two features that share a transport but nothing else:

- **Remote harness**: another host runs agents that join the board.
- **Remote operator surface**: the phone steers agents that run elsewhere.

## Protocol Landscape

ACP defines **stdio only**; Streamable HTTP is [a draft proposal in
progress](https://agentclientprotocol.com/protocol/v1/transports), and custom
transports are explicitly permitted as long as the JSON-RPC message format and
lifecycle survive. opencode has an open issue asking for ACP over WebSocket.
So corral cannot adopt a finished remote ACP transport today, and whatever frame
it defines between hub and spoke should stay JSON-RPC-shaped so the eventual
Streamable HTTP transport can replace it without touching the semantics.

[A2A](https://a2a-protocol.org/latest/) (Linux Foundation, 150-plus
organizations, HTTP/JSON, agent cards, task delegation) standardizes
communication between *vendor agent services*, not between a human's open
sessions. It is a poor fit for a personal session board and a good precedent to
cite for identity-per-endpoint.

## What This Implies for corral

Open questions the design must answer, in dependency order:

1. **Identity.** Physical location proves a directory only on one filesystem, so
   a remote session's identity must come from the authenticated peer host. That
   makes the trust unit a `(host, dir)` pair and demands an enrollment ceremony
   (pairing code, node key, tailnet identity) with a place to store it.
2. **Namespace.** Session ids and directory paths are unique per host only.
   Every record, whitelist line, roster entry and provenance tag grows a host
   component.
3. **Degradation.** Focus and spawn have no meaning across a link, and history
   export is a large transfer. The board must show clearly what a remote card
   cannot do, the way it already reports that history is unavailable on a
   dormant card.
4. **Anti-abuse.** Remote messaging needs the size cap, burst cap, loop
   suppression and queue bound that the local path never needed.
5. **The T13 rewrite.** "No ports, no network" becomes a scoped claim, and the
   source guard either dies or narrows to "corral creates no listener".
6. **Testing.** The VM e2e harness gains a second machine, or a namespace that
   simulates one.
