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

Test files: Layer A = `crates/daemon/tests/security.rs`; Layer B =
`crates/daemon/tests/live_daemon.rs`; the ack verdicts + roster are covered by
the in-process socket tests already in `crates/daemon/src/control.rs`.

| Claim | What must hold | How | Test |
|-------|----------------|-----|------|
| **T2** spoofed sender | `fromCwd` derived from the opened fd's real path; a symlink whose target is outside the outbox resolves there and is rejected (defeats a post-open redirect); location wins over any content `fromCwd` | A | `t2_cwd_from_location_not_content`, `t2_symlink_target_outside_outbox_rejected` |
| **T3** forged/cross-dir record | a record physically under dir A is attributed to A (content `cwd` ignored); `sessionId` charset + filename-match enforced; only vetted records reach `state/registry/` | A | `t3_record_attributed_to_physical_dir_ignoring_content_cwd`, `t3_bad_session_id_or_filename_mismatch_rejected` |
| **T4** unapproved launch command | an unregistered kind, or one whose launch-set (spawn/resume/gui/messageFlag) deviates, is quarantined (absent from `state/registry/`, returned pending); a matching registered kind publishes silently; a flood of novel labels all stay pending | A | `t4_unregistered_kind_is_quarantined_and_pending`, `t4_registered_kind_publishes_silently`, `t4_deviating_launch_set_is_a_new_pending_never_published`, `t4_flood_of_novel_labels_all_pending_none_published` |
| **T5** whitelist tampering | `state/` files live under `state/` (path-location assertion); the seal itself is sandbox-enforced | A + not self-testable | `seal_state_registry_created_0700` (paths); deny is deployment |
| **T6** control-socket rebind | a second bind on a live socket fails (the singleton guard); rebind-proofing of the parent is sandbox-enforced | B + not self-testable | `live_second_daemon_refuses_to_start` |
| **T7** forged provenance tag | delivered prompt's first line is the real `[from …]` tag; a `[from …]` embedded in the body is never at position zero | B | `live_whitelisted_message_delivered_with_positional_tag_and_audited` |
| **T13** no network | no crate names a TCP socket type (source guard, so a listener cannot be added silently) | A | `t13_no_tcp_listener_in_workspace` |
| **T14** confused-deputy submit | a real FIFO / dir / oversize / out-of-`outbox` path is rejected; a regular file under `<cwd>/.corral/outbox/` is accepted | A | `t14_rejects_fifo`, `t14_rejects_dir_and_oversize_and_outside_outbox`, `t2_cwd_from_location_not_content` (accept path) |
| **T15** connection flood | `MAX_CONCURRENT` silent connections held open never block the accept loop; a fresh connect still succeeds promptly | A (in-process `serve`) | `t15_flood_of_silent_connections_does_not_block_accept` |
| **T16** argument injection | `sessionId` charset gate rejects `--config=…` at `vet`; launch is an exec array, never a shell | unit + A | `discovery::session_id_charset_is_strict` (unit), `t16_vet_rejects_flag_like_session_id` |
| **T17** record aims at foreign socket | a record whose `socket` resolves outside its own `<D>/.corral/` is rejected by `vet`; an own-box socket is accepted | A | `t17_socket_must_resolve_inside_own_corral` |
| **seal modes** | `state/registry/` is created `0700` | A | `seal_state_registry_created_0700` |
| **singleton** | a second `corrald` refuses to start when the socket is live | B | `live_second_daemon_refuses_to_start` |
| **ack verdicts** | submit yields `accepted` / `approval_needed` / `recipient_not_found` / `directory_not_known` / `already_stopped` / `malformed` synchronously | unit (real socket) | `control::tests::*` (8 tests) |
| **roster confidentiality** | a `list` reply exposes a whitelisted dir's cwd, hides an unreachable one, keeps every session addressable by id | unit (real socket) | `control::tests::list_query_exposes_whitelisted_dir_and_hides_unreachable_paths` |
| **end-to-end deliver** | whitelisted submit → delivery lands on the recipient socket; a `routed to` line is appended to `audit.log` | B | `live_whitelisted_message_delivered_with_positional_tag_and_audited` |

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

## Prerequisite (done)

`crates/daemon` was bin-only, so its modules were unreachable from `tests/`.
Resolved: `crates/daemon/src/lib.rs` now exposes the modules and `main.rs` is a
thin `corral_daemon::run()` shell, so both layers reach the trust boundary
directly.
