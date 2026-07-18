# VM End-to-End Smoke Test Design

Date: 2026-07-18
Status: approved

## Goal

A reproducible, orchestrated-from-outside smoke test of the whole corral loop:
real harnesses (pi, opencode, claude, cursor) run inside a NixOS VM installed
exactly the way a user installs corral (`programs.corral.enable = true`), take
real agent turns against a deterministic stub LLM, and the Python test driver
asserts the full chain from adapter announce through board reflection, focus,
operator messaging, inter-agent messaging, stop, resume, and hidden agents.
Agents run confined under nono, so the test also proves the sandbox premise
SECURITY.md rests on: physical location = identity holds because an agent can
write only its own workdir plus the `~/.corral` allowlist.

The test is a living contract. It runs in CI and a hard rule in AGENTS.md binds
every adapter or user-facing behavior change to the matching scenario.

## Non-Goals

- No real LLM calls, no secrets, no network beyond the VM.
- No Cursor Composer turns (login-gated proprietary backend; documented
  residue).
- No board UI depth: the TUI/GUI get a render-and-act smoke slice, not a
  keystroke-complete exercise; UI behavior stays unit-tested in the crates.
- Not a benchmark; timing assertions use generous waits.

## Architecture

Four flake checks, one per harness scenario:

```
checks.e2e-pi        deep scenario: full corral loop on two pi sessions
checks.e2e-opencode  opencode announce/turns/delivery + cross-kind pi->opencode
checks.e2e-claude    claude sidecar lifecycle + hook-based delivery paths
checks.e2e-cursor    partial: extension announce/focus/state hooks, no Composer
```

All are `pkgs.testers.runNixOSTest` derivations under `nix/tests/`, sharing:

- `nix/tests/base.nix` — the base VM: sway (headless, software rendering) +
  kitty as the terminal, autologin user `alice`, home-manager (test-only flake
  input) applying corral's own `programs.corral.enable = true` module so the
  test validates the shipped install path (binaries, corrald user service,
  adapter symlinks). The corrald unit's `WantedBy` is retargeted from
  `graphical-session.target` to `default.target` in the test only, because a
  tty-launched sway does not activate the systemd graphical session. OCR
  (`enableOCR`) is on for the two board render assertions.
- `nix/tests/stub-llm/` — the deterministic model. `mock-llm`
  (dwmkerr/mock-llm, OpenAI-compatible, YAML rules with JMESPath matching and
  per-path sequences) runs as a systemd service on 127.0.0.1:6556 for pi and
  opencode. Claude Code speaks the Anthropic `/v1/messages` API with SSE
  streaming, which mock-llm does not emit, so a small dependency-free Python
  stub (`anthropic-stub.py`, stdlib http.server) serves 127.0.0.1:6557 for the
  claude scenario. Both select a canned script by matching the incoming user
  message, so the test chooses agent behavior by the prompt it sends:
  `smoke:reply` → plain text turn, `smoke:ask` → call the `question` tool,
  `smoke:msg <dir>` → call `corral_message_agent`, etc.
- `nix/tests/pkgs.nix` — test-only packages: pi and mock-llm built as npm
  meta-packages (a local `package.json` + committed `package-lock.json`
  depending on the published npm package, built with `buildNpmPackage`; this
  mirrors the common `npm install -g` setup without coupling to any private
  derivation), pinned by version + `npmDepsHash`.
- `nix/tests/profiles/agent.jsonc` — the vendored nono profile every agent
  launch goes through (`nono run --profile ...`): rw on the session cwd, rw on
  `~/.corral` (pointer store + control socket), read on the harness config, TCP
  connect to the stub port only. Vendored because `nono pull` needs the network
  the VM does not have.
- `nix/tests/pi-ext/` — two test-only pi extensions: `stub-provider.ts`
  registers an `openai-completions` provider pointing at the stub and the
  `smoke` model; `question.ts` registers a minimal blocking `question` tool
  (awaits a `ctx.ui` prompt) so corral-pi's requires_action path is
  exercisable (the question tool is not part of vanilla pi).
- `nix/tests/acp.py` — in-VM assertion helper: connects to a workdir socket,
  speaks newline JSON-RPC (initialize, session/list, waits for a given
  `state_update`), prints JSON for the test driver. Assertions read ground
  truth: `~/.corral/state/registry/` for vetted records, this helper for live
  state, `swaymsg -t get_tree` for windows and focus, OCR only to prove the
  boards visibly render.

## Scenarios and Assertions

### e2e-pi (the deep one)

Two pi sessions in kitty windows under sway, both launched under nono.

1. Announce: both records appear in `state/registry/` with label `pi`, socket
   set, cwd derived from physical location; pointer files exist in
   `input/registry/`.
2. Turns: prompt via ACP `session/prompt` (the operator `m` path);
   `state_update` goes running then idle; the record's `lastSeen` refreshes.
3. requires_action: a `smoke:ask` turn makes the model call `question`; state
   flips to requires_action; answering (typed into the focused kitty) returns
   it to idle.
4. Board TUI: `corral` in a third kitty; OCR sees a session title; Enter on
   the selected card focuses the pi window (`swaymsg` focused pid is the pi
   kitty); `m` composes and delivers (receiver transcript shows the text).
5. Inter-agent: a `smoke:msg` turn makes pi A call `corral_message_agent` to
   pi B's dir; with no whitelist the ack is approval_needed and nothing
   arrives; appending the pair to `~/.corral/whitelist` releases it (headless
   approval path); B receives the message with the `[from <dir> (session
   <id>)]` provenance tag.
6. Roster + stop: `list_corral_agents` returns both sessions;
   `corral_stop_agent` on a whitelisted pair kills B (record goes dormant,
   socket null).
7. Resume: corrald delivery to the dormant B resumes it with the message as
   first prompt (hidden by default → no new sway window, record `hidden`
   true); reveal via resume visible works.
8. Hidden spawn: a `force_new` dir-spawn runs inside cage; no sway window
   appears; the card exists and is messageable.
9. Sandbox negative: from inside pi A's nono sandbox, reading B's workdir
   record and writing `state/registry/` both fail; writing its own
   `.corral/` and the pointer dir succeed.
10. Board GUI: `corral-gui` starts (wgpu on the VM's software GL) and OCR sees
    a card title. If software rendering proves unable, this assertion is
    dropped with a dated note here.

### e2e-opencode

opencode (nixpkgs) with the corral-opencode plugin symlinked by the module.

1. Announce with label `opencode`, spawn/resume templates.
2. One stub turn: running → idle; activity string appears on the record's
   watch stream.
3. Operator delivery (`send_prompt`) lands in the session.
4. Cross-kind: a pi session in the same VM messages the opencode dir
   (whitelisted) and the text arrives.
5. Teardown: SIGTERM → record dormant (socket null).
6. Landlock outcome: the scenario first tries opencode under nono. If the
   bun-compiled binary SIGTRAPs (the known TODO.md landmine), the test records
   that as the expected outcome and reruns unsandboxed; either way the result
   is pinned, so a fix or regression shows up.

### e2e-claude

claude-code (nixpkgs, unfree) with `ANTHROPIC_BASE_URL` at the Anthropic stub
and a dummy `ANTHROPIC_API_KEY`.

1. SessionStart hook spawns the sidecar; record announces with the interactive
   Claude pid.
2. Turns: `UserPromptSubmit` → running, `Stop` → idle.
3. Live delivery: a corrald message to the busy session arrives via the
   Stop-block path and is visible in the transcript.
4. Idle delivery: a message to an idle session wakes it via asyncRewake and
   delivers on the next Stop.
5. Dormant delivery: `claude --resume <id>` with the message as trailing
   prompt.
6. SessionEnd (and the liveness probe) reap the sidecar; record goes dormant.

This scenario converts the adapter's UNVERIFIED list into assertions; whatever
fails is a real adapter bug to fix, not a test bug to paper over.

### e2e-cursor

code-cursor (nixpkgs, unfree) with the .vsix installed by the module.

1. Extension loads in the extension host; record announces `gui: true`, label
   `cursor`, socket named by the Electron pid.
2. ACP surface answers initialize + session/list.
3. Focus: the board's go raises the Cursor window (match by the record pid).
4. State hooks: invoking `state-hook.js` with simulated
   `beforeSubmitPrompt`/`stop` payloads drives running/idle.
5. Composer injection stays UNVERIFIED (documented residue): no login, no
   agent turns.

## Enforcement

- CI (GitHub Actions) splits into two jobs: the existing fast job (build +
  unit tests) on every push, and an `e2e` job running the four checks
  (`nix build .#checks.x86_64-linux.e2e-*`), allowed to be slow, unfree
  allowed only there via the tests' own nixpkgs import
  (`config.allowUnfree = true`). GitHub's standard runners expose /dev/kvm.
- AGENTS.md gains a hard rule beside TUI/GUI parity: any change to an adapter,
  the convention, or user-facing board/daemon behavior MUST update the
  matching `nix/tests/` scenario in the same change.
- `just e2e` runs all four locally; `just e2e-pi` etc. run one.

## Supply

- home-manager: new flake input, used only by `nix/tests/` (the package and
  dev shell do not depend on it).
- pi `0.80.10` and mock-llm: npm meta-packages under `nix/tests/npm/`,
  lockfiles committed, hashes pinned.
- nono, opencode, claude-code, code-cursor: from the already-pinned nixpkgs.
- cage/xwayland: already shipped by the corral package wrapper.

## Verify-in-VM List (expected iteration points)

Coded from docs, verified only by running the test:

- mock-llm YAML shapes for pi's `openai-completions` client (tool_calls
  encoding, streaming chunks).
- The Anthropic stub's SSE event sequence satisfying claude-code.
- nono profile field names/grants (validated with `nono profile schema`) and
  Landlock unix-socket + localhost-TCP behavior.
- pi settings/trust: the VM pre-seeds `~/.pi/agent/settings.json`
  (defaultModel = the stub model) and trust so first start is prompt-free.
- claude-code hook payload field names (the adapter's own UNVERIFIED list).
- corral-gui under the VM's software renderer.

## Rejected Alternatives

- Real API keys / impure test: not reproducible, not CI-safe.
- One VM covering all harnesses: one giant serial test; per-harness checks
  isolate failures and parallelize.
- Bespoke OpenAI stub: mock-llm already does rules/sequences/streaming and is
  maintained; we only hand-write the Anthropic stub it lacks.
- Reusing the maintainer's private pi derivation: couples the repo to a
  private tree; the npm meta-package is the common installation shape.
- Skipping nono: would leave the security model's central premise untested.
