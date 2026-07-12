//! Window focus seam. The triage core calls `WindowFocuser::focus`; the sway
//! implementation is the only place that knows about the compositor.
//!
//! Correlation: sway knows the *terminal* process, not the pi session. Bridge
//! by a `/proc` parent-walk: the socket's pid, walked up its `PPid` chain, hits
//! the terminal process whose pid sway reports for the window. This holds
//! because the pi sandbox (nono/bwrap) does not unshare the PID namespace.
//!
//! The walk must not reach corral's own terminal, or it could focus/close the
//! board itself. Board-spawned windows are detached from corral (see
//! `launch.rs`, `setsid --fork`), so the chain terminates at `pi -> kitty ->
//! init`; manually launched agents were never corral's descendants.

use std::collections::HashSet;
use std::process::Command;

use crate::model::Agent;

/// Manage an agent's terminal window. `focus` raises it; `close` shuts it
/// (which ends the session). Both are window operations, so both live behind
/// this compositor seam.
pub trait WindowFocuser {
    fn focus(&self, agent: &Agent) -> Result<(), String>;
    fn close(&self, agent: &Agent) -> Result<(), String>;
}

pub struct SwayFocuser;

impl SwayFocuser {
    /// The agent's window as sway sees it: its con id (for focus) and the
    /// pid sway reports for it (the terminal process, for close). Found by the
    /// `/proc` parent-walk from the socket pid up to the window's pid.
    fn window(&self, agent: &Agent) -> Result<(i64, u32), String> {
        let ancestors = ancestor_pids(agent.pid);
        let tree = sway_get_tree()?;
        find_window(&tree, &ancestors)
            .ok_or_else(|| format!("no sway window found for pid {}", agent.pid))
    }
}

impl WindowFocuser for SwayFocuser {
    fn focus(&self, agent: &Agent) -> Result<(), String> {
        let (con_id, _) = self.window(agent)?;
        let ok = Command::new("swaymsg")
            .arg(format!("[con_id={con_id}] focus"))
            .status()
            .map_err(|e| format!("swaymsg failed: {e}"))?
            .success();
        if ok {
            Ok(())
        } else {
            Err("swaymsg focus returned non-zero".into())
        }
    }

    // Kill the terminal process itself (the pid sway reports for the window),
    // not a window-close request: kitty's `confirm_os_window_close` would keep
    // an interactive window open on a close request. Killing the process takes
    // the window and pi with it; pi's socket then goes unreachable, so the
    // session reappears as a dormant, resumable record on the next scan.
    fn close(&self, agent: &Agent) -> Result<(), String> {
        let (_, window_pid) = self.window(agent)?;
        let ok = Command::new("kill")
            .arg(window_pid.to_string())
            .status()
            .map_err(|e| format!("kill failed: {e}"))?
            .success();
        if ok {
            Ok(())
        } else {
            Err("kill returned non-zero".into())
        }
    }
}

/// The pid itself plus every ancestor, walking `/proc/<pid>/stat` PPid links.
/// Stops at pid 1 or the first missing/unreadable entry.
fn ancestor_pids(start: u32) -> HashSet<u32> {
    let mut set = HashSet::new();
    let mut pid = start;
    while pid > 1 && set.insert(pid) {
        match ppid_of(pid) {
            Some(ppid) => pid = ppid,
            None => break,
        }
    }
    set
}

/// Parent pid from `/proc/<pid>/stat`. Field 4 (1-indexed) is PPid; the comm
/// field (2) may contain spaces and parentheses, so split on the last ')'.
fn ppid_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    // after_comm starts with " <state> <ppid> ..."
    after_comm.split_whitespace().nth(1)?.parse().ok()
}

/// Walk the sway tree JSON for the first node whose `pid` is in `pids`,
/// returning its con `id` and that `pid`.
fn find_window(node: &serde_json::Value, pids: &HashSet<u32>) -> Option<(i64, u32)> {
    if let Some(pid) = node.get("pid").and_then(|p| p.as_u64()) {
        if pids.contains(&(pid as u32)) {
            if let Some(id) = node.get("id").and_then(|i| i.as_i64()) {
                return Some((id, pid as u32));
            }
        }
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(|c| c.as_array()) {
            for child in children {
                if let Some(found) = find_window(child, pids) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn sway_get_tree() -> Result<serde_json::Value, String> {
    let out = Command::new("swaymsg")
        .args(["-t", "get_tree"])
        .output()
        .map_err(|e| format!("swaymsg get_tree failed: {e}"))?;
    serde_json::from_slice(&out.stdout).map_err(|e| format!("bad sway tree json: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_window_by_pid_in_tree() {
        let tree = serde_json::json!({
            "id": 1, "nodes": [
                {"id": 5, "pid": 4834, "name": "keepass"},
                {"id": 46, "nodes": [{"id": 66, "pid": 84669, "name": "pi"}]}
            ]
        });
        let mut pids = HashSet::new();
        pids.insert(84669);
        assert_eq!(find_window(&tree, &pids), Some((66, 84669)));
    }

    #[test]
    fn no_match_yields_none() {
        let tree = serde_json::json!({"id": 1, "nodes": [{"id": 5, "pid": 1}]});
        let mut pids = HashSet::new();
        pids.insert(9999);
        assert_eq!(find_window(&tree, &pids), None);
    }
}
