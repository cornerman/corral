# Security Test Matrix

Maps every enforceable claim in [SECURITY.md](../SECURITY.md) to the test that
assures it. "Complete" means every `[in place]` claim has a row here that either
names a test or states, with a reason, that the claim is not self-testable.

Legend for **How**:

- `unit` — pure `#[cfg(test)]` test already in the module (logic only).
- `A` — in-process integration test over a real tmpfs / real unix socket
  (`crates/daemon/tests/`, `crates/core/tests/`). Exercises real filesystem and
  socket semantics the mitigation depends on.
- `B` — spawns the real `corrald` binary plus a fake agent listener
  (`crates/daemon/tests/live_*.rs`). Proves the wiring `main`/`run` does.
- `not self-testable` — an OS/sandbox property corral cannot assert about
  itself; the suite asserts the corral-side half only, the rest is deployment.

| Claim | What must hold | How | Test |
|-------|----------------|-----|------|
| **T2** spoofed sender | `fromCwd` derived from the open fd's `/proc/self/fd`; a symlink swapped **after open** cannot redirect it; `fromSession` must match a curated record in that dir | A | `t2_outbox_symlink_swap_after_open`, `t2_from_session_must_be_curated` |
| **T3** forged/cross-dir record | record physically under dir A is attributed to A; content has no `cwd`; `sessionId` charset + filename-match enforced; only vetted records reach `state/registry/` | A | `t3_record_attributed_to_physical_dir`, `t3_bad_session_id_rejected`, `t3_forged_record_absent_from_vetted` |
| **T4** unapproved launch command | an unregistered kind, or one whose launch-set (spawn/resume/gui/messageFlag) deviates, is quarantined (absent from `state/registry/`, returned pending); a matching registered kind publishes silently | A | `t4_unregistered_quarantined`, `t4_deviating_set_is_new_pending`, `t4_registered_publishes_silently`, `t4_flood_of_labels_all_pending` |
| **T5** whitelist tampering | `state/` files live under `state/`, not the agent-writable index dir (path-location assertion) | A + not self-testable | `t5_state_paths_under_state_dir` (seal itself is sandbox-enforced) |
| **T6** control-socket rebind | socket path is at the `~/.corral` root, parent is not the agent-writable registry dir; a second bind on a live socket fails | A + B | `t6_socket_at_corral_root`, `live_second_bind_refused` |
| **T7** forged provenance tag | delivered prompt's first line is the real `[from …]` tag; a `[from …]` embedded in the body is not at position zero | A + B | `t7_provenance_is_first_line`, `live_delivery_tag_precedes_body` |
| **T13** no network | no TCP listener is opened by any binary (grep guard + a bind-probe over the run loop) | A | `t13_no_tcp_listener` |
| **T14** confused-deputy submit | a real FIFO / device / dir / oversize / out-of-`outbox` path is rejected; a regular file under `<cwd>/.corral/outbox/` is accepted | A | `t14_rejects_fifo`, `t14_rejects_oversize`, `t14_rejects_path_outside_outbox`, `t14_accepts_regular_outbox_file` |
| **T15** connection flood | with `MAX_CONCURRENT` slow connections held open, a fresh valid request still gets answered | B | `live_flood_does_not_block_serving` |
| **T16** argument injection | `sessionId` charset gate rejects `--config=…`; a `{cwd}` with a leading `-`/`@`/space is guarded; launch is an exec array, never a shell | unit + A | `t16_session_id_charset` (unit), `t16_cwd_argv_guarded` |
| **T17** record aims at foreign socket | a record whose `socket` resolves outside its own `<D>/.corral/` is rejected by `vet`; a card can only drive a session in its own box | A | `t17_socket_must_be_under_own_corral` |
| **seal modes** | `state/registry/` and the socket parent are created `0700` | A | `seal_dirs_are_0700` |
| **singleton** | a second `corrald` refuses to start when the socket is live | B | `live_second_daemon_refused` |
| **ack verdicts** | submit yields `accepted` / `approval_needed` / `recipient_not_found` / `directory_not_known` synchronously per resolved facts | unit + B | `mailbox::classify` (unit), `live_ack_recipient_not_found` |
| **end-to-end deliver** | whitelisted submit → delivery lands on the recipient socket; a decision is appended to `audit.log` | B | `live_whitelisted_message_delivered`, `live_audit_line_written` |

## Not self-testable (documented, asserted where possible)

- **T1** sibling socket unreachable — enforced by the whole-process workdir
  sandbox, an OS property. The suite asserts only that sockets are
  workdir-local by path (`discovery` unit tests already cover the filename
  grammar).
- **T5 / T6 seal** — the *unwritability* of `state/` and the socket parent is
  the sandbox allowlist's job, not corral's. Asserted: the paths are located
  where the seal expects (above). The deny itself is deployment.
- **T9** pid-based focus/kill unverified — accepted by design; there is nothing
  to assert beyond "the pid is taken from the filename verbatim", which is the
  documented behavior, not a defense.
- **Load-bearing precondition (#42)** — a whole-process sandbox that boxes each
  agent; corral cannot detect its absence (it sees only file locations). No
  test can stand in for it; SECURITY.md states it as a deployment precondition.

## Prerequisite

`crates/daemon` is bin-only today, so its modules are unreachable from
`tests/`. Step zero: add `crates/daemon/src/lib.rs` (`pub mod` for
control/curator/router/mailbox/registrations/notify/tray/icon) and reduce
`main.rs` to a thin `corral_daemon::run()` shell.
