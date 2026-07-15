# Drive Agent State by Moving Cards

Date: 2026-07-15

## Goal

Let the operator change a live agent's turn state by moving its card between the
board's columns — shift+arrow (keyboard) or drag/drop (mouse). Moving a card is
a *trigger*: corral fires the real agent action and the card relocates only when
the agent's own `state_update` confirms it. The board stays a truthful reflector
(map = territory); it never paints a state the agent has not reached.

## Prerequisite (done)

`watch::run` seeded every new agent as `DEFAULT_STATE` (Idle) and dropped the
extension's connect-time `state_update` (it arrives before the `session/list`
reply, so `Board::apply` had no agent to update yet). Fixed: stash the pre-seed
state and stamp it onto the `Upsert`. Without a truthful starting state the move
feature would mis-fire. Commit: "watch: seed initial state from pre-Upsert
state_update, not Idle default".

## Interaction (Model A, both shells)

Grab a card (mouse drag, or shift+arrow on the selection) → **move mode**: the
board dims and each column renders as a labeled **drop-box** with its inner
cards hidden (no visual noise, sidesteps any "which slot" choice). Requires
Action renders as a greyed, non-selectable box — never a destination.

- Shift+arrow slides the ghost box across columns; **shift-release commits**
  (kitty keyboard protocol, same requirement as Shift+Enter; absent kitty the
  keyboard move is unavailable and mouse drag still works). The GUI uses iced's
  native key-release and drag events.
- Mouse **drop commits**; Esc or drop-outside cancels.
- The slot is never operator-chosen. On commit the card would land at the top of
  the destination column, but it does not land until confirmed (below).

## Transition table (source → destination)

| From \ To | Requires Action | Idle | Running | Dormant |
|---|:--:|:--:|:--:|:--:|
| Requires Action | — | cancel question | no-op | kill |
| Idle | no-op | — | nudge `continue` | kill |
| Running | no-op | cancel turn | — | kill |
| Dormant | no-op | resume | resume + nudge | — |

- **cancel** = `session/cancel` (aborts the turn; also unblocks a pending
  `question`). **nudge** = `session/prompt` with literal text `"continue"`
  (corral-pi rejects an empty prompt; the word is honest in the transcript).
  **kill** = kill the window pid (today's `d`; pi goes dormant/resumable).
  **resume** = run `resumeCommand`. **resume + nudge** = resume-with-message
  `"continue"` (existing launch-with-message path).
- **Requires Action is never a destination**: corral cannot make an agent open a
  question. Every `→ Requires Action` move is a no-op.

## Commit → pending → confirm

On commit corral fires the action and marks the agent **pending → \<target\>**
with a timestamp. The card stays in its real column with an in-flight badge
(`→ Idle ⋯`). A `state_update` (or dormant/live registry transition) matching the
target clears the badge; the card is then naturally in the new column. No
confirmation within ~5s clears the badge and leaves the card unchanged
(fail-quiet; the action was fire-and-forget). Kill and resume confirm via the 1s
registry scan (socket disappears / appears). No confirmation dialog on kill
(parity with `d`).

## Architecture

- `core::move` (new, pure, unit-tested): `action_for(from, to) -> MoveAction`
  and `confirms(action, state) -> bool`. Single source of the table both shells
  consume.
- `core::prompt`: add `send_cancel(socket)` beside `send_prompt`. Nudge reuses
  `send_prompt`; resume/kill reuse existing `launch` / `kill_pid`.
- `crates/board` (`ui.rs` + `main.rs`): move-mode state machine, drop-box
  rendering, shift+arrow / drag dispatch, pending badge.
- `crates/gui` (`dashboard.rs`): same via iced drag + key-release; base16
  drop-box styling. TUI/GUI parity is a hard rule.
- No adapter change expected (reuses the existing ACP surface). One flag: verify
  `ctx.abort()` in corral-pi actually unblocks a pending `question`; if not,
  corral-pi cancels the question explicitly on `session/cancel`.

## Known limitations

- `cancel` is a no-op on the Claude and Cursor adapters, so cancel-moves degrade
  there (nudge/kill/resume still work).
- The nudge sends literal `"continue"` (empty prompts rejected); visible in the
  transcript by design.

## Testing

Pure `core::move` table + `confirms` unit-tested exhaustively. `send_cancel`
tested against a throwaway listener (like `send_prompt`). Move-mode state machine
(enter / slide / commit / cancel / no-op rejection) unit-tested where the logic
is pure, per shell.
