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
//!
//! A GUI agent (record `gui: true`, e.g. quine) draws its own window carrying
//! its own pid, so `match_pids` matches strictly on the socket pid and skips
//! the parent-walk entirely: climbing would otherwise pick up the launching
//! terminal's window and target the wrong one.

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

/// Pick a focuser for the running session: EWMH on X11 (any compliant WM,
/// keyed on pid), else sway on Wayland (its IPC also reports pid). A Wayland
/// session that is not sway currently has no focuser; other compositors drop
/// in behind this seam. Selection is by environment: `$WAYLAND_DISPLAY` marks
/// a Wayland session, `$DISPLAY` an X11 one.
#[derive(Debug, PartialEq, Eq)]
enum FocuserKind {
    X11,
    Sway,
    Hyprland,
    Niri,
    /// No adapter for this environment (e.g. GNOME/KDE on Wayland).
    Unsupported,
}

/// Classify the session from environment-variable presence (pure). Wayland
/// compositor markers win first (a compositor also exports `DISPLAY` for
/// Xwayland, but its windows are Wayland-native, so EWMH would not find them);
/// a pure X11 session (no `$WAYLAND_DISPLAY`) uses EWMH; anything else
/// (GNOME/KDE Wayland, headless) has no adapter.
fn detect_kind(has: impl Fn(&str) -> bool) -> FocuserKind {
    if has("HYPRLAND_INSTANCE_SIGNATURE") {
        FocuserKind::Hyprland
    } else if has("NIRI_SOCKET") {
        FocuserKind::Niri
    } else if has("SWAYSOCK") {
        FocuserKind::Sway
    } else if !has("WAYLAND_DISPLAY") && has("DISPLAY") {
        FocuserKind::X11
    } else {
        FocuserKind::Unsupported
    }
}

pub fn detect() -> Box<dyn WindowFocuser> {
    match detect_kind(|k| std::env::var_os(k).is_some()) {
        FocuserKind::X11 => Box::new(X11Focuser),
        FocuserKind::Sway => Box::new(SwayFocuser),
        FocuserKind::Hyprland => Box::new(HyprlandFocuser),
        FocuserKind::Niri => Box::new(NiriFocuser),
        FocuserKind::Unsupported => Box::new(UnsupportedFocuser),
    }
}

/// The focuser for a session corral has no adapter for: every operation fails
/// loud with a message the shell surfaces in its status line, rather than
/// silently guessing a compositor. GNOME/KDE on Wayland land here (they need a
/// Shell extension / KWin script); they focus fine under X11.
pub struct UnsupportedFocuser;

impl UnsupportedFocuser {
    fn err() -> String {
        "no window focuser for this session (supported: X11, sway, Hyprland, niri; \
         not GNOME/KDE on Wayland)"
            .into()
    }
}

impl WindowFocuser for UnsupportedFocuser {
    fn focus(&self, _agent: &Agent) -> Result<(), String> {
        Err(Self::err())
    }
    fn close(&self, _agent: &Agent) -> Result<(), String> {
        Err(Self::err())
    }
}

pub struct SwayFocuser;

impl SwayFocuser {
    /// The agent's window as sway sees it: its con id (for focus) and the
    /// pid sway reports for it (the terminal process, for close). Found by the
    /// `/proc` parent-walk from the socket pid up to the window's pid.
    fn window(&self, agent: &Agent) -> Result<(i64, u32), String> {
        let pids = match_pids(agent);
        let tree = sway_get_tree()?;
        find_window(&tree, &pids)
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

/// The pids whose window identifies this agent. A GUI agent (e.g. quine)
/// draws its own window carrying its own pid (= the socket pid), so match
/// strictly on it: climbing the parent chain would, for a terminal-launched
/// GUI app, also pick up the launching terminal's window and focus (or on
/// close, kill) the wrong one. A terminal agent (pi) owns no window itself,
/// so its window is the terminal up the ancestor chain.
fn match_pids(agent: &Agent) -> HashSet<u32> {
    if agent.gui {
        std::iter::once(agent.pid).collect()
    } else {
        ancestor_pids(agent.pid)
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

// --- Wayland: per-compositor IPC (the only path that reports pid) ---

/// Kill a window's process, the shared `close` mechanism: a compositor
/// close request is refused by kitty's confirm-on-close, but killing the
/// process takes the window and agent with it, leaving a dormant record.
fn kill_pid(pid: u32) -> Result<(), String> {
    let ok = Command::new("kill")
        .arg(pid.to_string())
        .status()
        .map_err(|e| format!("kill failed: {e}"))?
        .success();
    if ok {
        Ok(())
    } else {
        Err("kill returned non-zero".into())
    }
}

/// Run a JSON-emitting IPC command and parse its stdout as an array.
fn json_array(program: &str, args: &[&str]) -> Result<Vec<serde_json::Value>, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program} failed: {e}"))?;
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .ok_or_else(|| format!("{program}: expected a JSON array"))
}

/// The first window entry whose `pid` is the agent's pid or an ancestor (the
/// terminal owning the window is an ancestor of the agent's socket pid).
fn entry_for_pid<'a>(
    list: &'a [serde_json::Value],
    ancestors: &HashSet<u32>,
) -> Option<&'a serde_json::Value> {
    list.iter().find(|e| {
        e.get("pid")
            .and_then(|p| p.as_u64())
            .is_some_and(|p| ancestors.contains(&(p as u32)))
    })
}

/// Hyprland via `hyprctl`: match the agent's pid in `clients -j`, then focus by
/// the window's stable address (which also switches to its workspace).
pub struct HyprlandFocuser;

impl WindowFocuser for HyprlandFocuser {
    fn focus(&self, agent: &Agent) -> Result<(), String> {
        let clients = json_array("hyprctl", &["clients", "-j"])?;
        let pids = match_pids(agent);
        let addr = entry_for_pid(&clients, &pids)
            .and_then(|e| e.get("address").and_then(|a| a.as_str()))
            .ok_or_else(|| format!("no hyprland window for pid {}", agent.pid))?;
        let ok = Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &format!("address:{addr}")])
            .status()
            .map_err(|e| format!("hyprctl failed: {e}"))?
            .success();
        if ok {
            Ok(())
        } else {
            Err("hyprctl focuswindow returned non-zero".into())
        }
    }

    fn close(&self, agent: &Agent) -> Result<(), String> {
        let clients = json_array("hyprctl", &["clients", "-j"])?;
        let pids = match_pids(agent);
        let pid = entry_for_pid(&clients, &pids)
            .and_then(|e| e.get("pid").and_then(|p| p.as_u64()))
            .ok_or_else(|| format!("no hyprland window for pid {}", agent.pid))?;
        kill_pid(pid as u32)
    }
}

/// niri via `niri msg`: match the agent's pid in the window list, then focus by
/// the window id (which also switches to its workspace).
pub struct NiriFocuser;

impl WindowFocuser for NiriFocuser {
    fn focus(&self, agent: &Agent) -> Result<(), String> {
        let windows = json_array("niri", &["msg", "--json", "windows"])?;
        let pids = match_pids(agent);
        let id = entry_for_pid(&windows, &pids)
            .and_then(|e| e.get("id").and_then(|i| i.as_u64()))
            .ok_or_else(|| format!("no niri window for pid {}", agent.pid))?;
        let ok = Command::new("niri")
            .args(["msg", "action", "focus-window", "--id", &id.to_string()])
            .status()
            .map_err(|e| format!("niri failed: {e}"))?
            .success();
        if ok {
            Ok(())
        } else {
            Err("niri focus-window returned non-zero".into())
        }
    }

    fn close(&self, agent: &Agent) -> Result<(), String> {
        let windows = json_array("niri", &["msg", "--json", "windows"])?;
        let pids = match_pids(agent);
        let pid = entry_for_pid(&windows, &pids)
            .and_then(|e| e.get("pid").and_then(|p| p.as_u64()))
            .ok_or_else(|| format!("no niri window for pid {}", agent.pid))?;
        kill_pid(pid as u32)
    }
}

// --- X11 (EWMH) ---

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, PropMode, Window, CLIENT_MESSAGE_EVENT,
};
use x11rb::wrapper::ConnectionExt as _; // change_property8

/// Focus X11 windows through EWMH, so one implementation drives every
/// compliant window manager (i3, bspwm, openbox, X11 KWin/Mutter, …),
/// workspace-switching included: activating a window makes a compliant WM
/// switch to its workspace, raise it, and focus it. Correlation is by pid via
/// `_NET_WM_PID`, the same reliable signal as the sway path (the window's pid
/// is an ancestor of the agent's socket pid).
pub struct X11Focuser;

impl WindowFocuser for X11Focuser {
    fn focus(&self, agent: &Agent) -> Result<(), String> {
        let (conn, root, win) = self.find(agent)?;
        let active = intern(&conn, b"_NET_ACTIVE_WINDOW")?;
        // Source indication 2 = pager: corral is a taskbar-like tool, so this
        // bypasses focus-stealing prevention that would otherwise only flash
        // urgency. A real server timestamp (not CurrentTime) is required for
        // the same reason; `server_time` fetches one via a property round-trip.
        let time = server_time(&conn, root)?;
        let data = [2u32, time, 0, 0, 0];
        let event = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: active,
            data: data.into(),
        };
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            event,
        )
        .map_err(|e| format!("x11 send_event: {e}"))?;
        conn.flush().map_err(|e| format!("x11 flush: {e}"))?;
        Ok(())
    }

    // Kill the window's process (its `_NET_WM_PID`), matching the sway path:
    // a close request would be refused by kitty's confirm-on-close, but
    // killing the process takes the window and the agent with it, leaving a
    // dormant resumable record.
    fn close(&self, agent: &Agent) -> Result<(), String> {
        let (conn, _root, win) = self.find(agent)?;
        let pid = window_pid(&conn, win).ok_or("x11: window has no _NET_WM_PID")?;
        kill_pid(pid)
    }
}

impl X11Focuser {
    /// Connect and find the agent's window: the first client whose
    /// `_NET_WM_PID` is the agent's pid or one of its ancestors (the terminal
    /// that owns the window is an ancestor of the agent's socket pid).
    fn find(
        &self,
        agent: &Agent,
    ) -> Result<(x11rb::rust_connection::RustConnection, Window, Window), String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("x11 connect: {e}"))?;
        let root = conn.setup().roots[screen].root;
        let pids = match_pids(agent);
        let list = intern(&conn, b"_NET_CLIENT_LIST")?;
        let windows = conn
            .get_property(false, root, list, AtomEnum::WINDOW, 0, u32::MAX)
            .map_err(|e| format!("x11 client list: {e}"))?
            .reply()
            .map_err(|e| format!("x11 client list reply: {e}"))?;
        let windows = windows
            .value32()
            .ok_or("x11: _NET_CLIENT_LIST not 32-bit")?;
        for win in windows {
            if let Some(pid) = window_pid(&conn, win) {
                if pids.contains(&pid) {
                    return Ok((conn, root, win));
                }
            }
        }
        Err(format!("no x11 window found for pid {}", agent.pid))
    }
}

fn intern(conn: &impl Connection, name: &[u8]) -> Result<u32, String> {
    conn.intern_atom(false, name)
        .map_err(|e| format!("x11 intern: {e}"))?
        .reply()
        .map_err(|e| format!("x11 intern reply: {e}"))
        .map(|r| r.atom)
}

/// The `_NET_WM_PID` of a window, if set.
fn window_pid(conn: &impl Connection, win: Window) -> Option<u32> {
    let atom = intern(conn, b"_NET_WM_PID").ok()?;
    let reply = conn
        .get_property(false, win, atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    reply.value32().and_then(|mut v| v.next())
}

/// A current X server timestamp, obtained the canonical way: a zero-length
/// append to a property on the root window generates a `PropertyNotify` whose
/// `time` is the server's current time. Needed so `_NET_ACTIVE_WINDOW` is not
/// deferred by focus-stealing prevention.
fn server_time(conn: &impl Connection, root: Window) -> Result<u32, String> {
    use x11rb::protocol::Event;
    // Select property changes on the root, then trigger one on a private atom.
    let marker = intern(conn, b"_CORRAL_TIMESTAMP")?;
    conn.change_window_attributes(
        root,
        &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
            .event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| format!("x11 select prop: {e}"))?;
    conn.change_property8(PropMode::APPEND, root, marker, AtomEnum::STRING, &[])
        .map_err(|e| format!("x11 mark: {e}"))?;
    conn.flush().map_err(|e| format!("x11 flush: {e}"))?;
    loop {
        match conn
            .wait_for_event()
            .map_err(|e| format!("x11 event: {e}"))?
        {
            Event::PropertyNotify(ev) if ev.atom == marker => return Ok(ev.time),
            _ => continue,
        }
    }
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

    #[test]
    fn entry_for_pid_matches_hyprland_and_niri_shapes() {
        // hyprctl clients -j: address + pid; niri msg windows: id + pid.
        let hypr = serde_json::json!([
            {"address": "0xaaa", "pid": 10},
            {"address": "0xbbb", "pid": 84669}
        ]);
        let niri = serde_json::json!([
            {"id": 1, "pid": 10},
            {"id": 7, "pid": 84669}
        ]);
        let mut pids = HashSet::new();
        pids.insert(84669);
        assert_eq!(
            entry_for_pid(hypr.as_array().unwrap(), &pids)
                .and_then(|e| e.get("address").and_then(|a| a.as_str())),
            Some("0xbbb")
        );
        assert_eq!(
            entry_for_pid(niri.as_array().unwrap(), &pids)
                .and_then(|e| e.get("id").and_then(|i| i.as_u64())),
            Some(7)
        );
        pids.clear();
        pids.insert(999);
        assert_eq!(entry_for_pid(niri.as_array().unwrap(), &pids), None);
    }

    #[test]
    fn gui_agent_matches_only_its_own_pid() {
        use crate::model::{Origin, State};
        let mk = |gui: bool, pid: u32| Agent {
            socket_path: std::path::PathBuf::from("/s.sock"),
            pid,
            label: "quine".into(),
            session_id: None,
            title: None,
            cwd: None,
            state: State::Idle,
            origin: Origin::Live,
            spawn_command: None,
            resume_command: None,
            activity: None,
            gui,
            message_flag: None,
            hidden: false,
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
            state_since: std::time::Instant::now(),
            last_activity: std::time::Instant::now(),
        };
        let me = std::process::id();
        // GUI: strictly its own pid, never climbing to a parent window.
        assert_eq!(
            match_pids(&mk(true, me)),
            std::iter::once(me).collect::<HashSet<u32>>()
        );
        // Terminal agent: the ancestor set, which at least contains itself
        // (and, for a real process under a parent, more).
        let term = match_pids(&mk(false, me));
        assert!(term.contains(&me));
    }

    #[test]
    fn detect_kind_covers_all_sessions() {
        let with = |present: &'static [&'static str]| move |k: &str| present.contains(&k);
        // Wayland compositor markers win (even alongside DISPLAY from Xwayland).
        assert_eq!(
            detect_kind(with(&[
                "HYPRLAND_INSTANCE_SIGNATURE",
                "WAYLAND_DISPLAY",
                "DISPLAY"
            ])),
            FocuserKind::Hyprland
        );
        assert_eq!(
            detect_kind(with(&["NIRI_SOCKET", "WAYLAND_DISPLAY"])),
            FocuserKind::Niri
        );
        assert_eq!(
            detect_kind(with(&["SWAYSOCK", "WAYLAND_DISPLAY"])),
            FocuserKind::Sway
        );
        // Pure X11 session.
        assert_eq!(detect_kind(with(&["DISPLAY"])), FocuserKind::X11);
        // GNOME/KDE Wayland: WAYLAND_DISPLAY but no known marker -> unsupported
        // (DISPLAY from Xwayland must not fool us into EWMH).
        assert_eq!(
            detect_kind(with(&["WAYLAND_DISPLAY", "DISPLAY"])),
            FocuserKind::Unsupported
        );
        // Headless / nothing.
        assert_eq!(detect_kind(with(&[])), FocuserKind::Unsupported);
    }
}
