# Plan — Label Override via `corral_message_agent`

## Goal

Let the caller of `corral_message_agent` choose which agent kind (pi / opencode
/ …) is started when a `target_dir` message has to **spawn** a fresh agent. Add
one optional tool parameter, `label`. Omitted → today's behavior (the dir's
existing record). Unknown label → fail loud (no arbitrary kind).

Scope is only the override + its resolution. The most-used default, the
whitelist-scoped discovery, and never-announced-dir spawning are separate OPEN
items in TODO.md; this plan is written so it composes with them (global label
catalog, not dir-scoped).

## Term

Use `label` end to end — the tool parameter, the wire key, the `Message` field,
and the registry record all say `label`, matching the existing model /
discovery / card-badge field. No new synonym (`harness`/`tool`) and no rename;
one word, one meaning.

## Where `label` is consulted

Only on the **spawn** path of a directory target. If a live agent already
works in the dir it is reused (its kind is fixed) and `label` is ignored;
`force_new` (or no live agent) is what triggers a spawn, and only then does
`label` pick the kind. Documented as a deliberate limit.

## Resolution rule

`spawn_command_for_label(entries, label)` = the `spawnCommand` of the first
registry record (any dir, live or dormant) whose `label` matches. Global,
not dir-scoped, so a kind seen anywhere can be started in any dir (composes
with never-announced-dir spawning). No catalog match → the router returns a
loud error status and does NOT spawn.

## Ack caveat (accepted)

The synchronous ack (`mailbox::classify` in `control.rs`) fires before routing
and knows only target-cwd + whitelist. An unknown `label` is discovered later
at spawn time in the router, so it cannot be reflected in the ack — it surfaces
only as the router's status string (logged), consistent with the existing
fire-and-forget, no-delivery-ack contract. v1 accepts this. (A later option:
validate `label` against the registry catalog at ack time in `control.rs`,
adding a `label_not_known` ack — deferred.)

## Steps (each compiles + tests green before the next)

1. **mailbox.rs** — add `label: Option<String>` to `Message`; `parse_message`
   reads the `label` key (absent → `None`). Update the two existing test
   fixtures. New test: `parses_label_when_present` + absent → `None`.

2. **router.rs** — add `spawn_command_for_label(entries, &str)`. In
   `deliver_dir`, on the spawn branch: if `msg.label` is `Some`, resolve via
   `spawn_command_for_label`; else keep `spawn_command_for_dir`. If a label
   was given but unresolved, return `format!("route spawn: unknown label {l}")`
   and do not launch. Tests: (a) `label` selects that kind even when the dir's
   own records are a different kind; (b) unknown label → error status,
   `StubLauncher.spawns == 0`; (c) no `label` → unchanged behavior.

3. **extensions** (`corral-announce.ts` and `corral-opencode.ts`, mirror) — add
   `label: Type.Optional(Type.String({ description: "…" }))` to the tool
   parameters; when present, set `record.label = params.label`. Extend the
   tool description: "Optionally set `label` (pi/opencode/…) to choose which
   agent kind to start when a new one is spawned for `target_dir`." UNVERIFIED
   in sandbox (no TS toolchain) — flag in-file, matching existing practice.

4. **docs** — AGENTS.md (Extensions + Inter-Agent Messaging), CONVENTION.md
   (Appendix A message fields), README if it lists the tool params; flip the
   TODO.md OPEN label-on-spawn bullet to reference this as the override half (the
   most-used default stays open).

## Out of scope (stays OPEN in TODO.md)

- Most-used-label default when `label` omitted.
- Whitelist-scoped session discovery (`list` over the control socket).
- Never-announced-dir spawn (uses given dir as cwd) — independent, composes.
- Ack-time label validation (`label_not_known`).
