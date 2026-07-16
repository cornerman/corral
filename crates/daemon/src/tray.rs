//! The system-tray surface (`ksni`/StatusNotifierItem). The tray is the
//! daemon's always-present control surface: it shows whether a message is
//! waiting for approval and lets the operator Allow once / Allow always / Deny
//! right there, plus open the board or quit. It is the reliable approval path
//! (the desktop notification is a convenience mirror that may be lost if no
//! notification daemon runs).
//!
//! The tray thread cannot touch the router directly, so every operator action
//! becomes a `TrayCommand` on a channel the main loop drains. Best-effort: if
//! no StatusNotifierHost is running (`spawn` fails), the daemon keeps routing
//! and falls back to notifications only.

use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};

use ksni::blocking::TrayMethods;

use crate::router::ApprovalAction;

/// An operator action from the tray for the main loop to act on.
pub enum TrayCommand {
    /// Decide the pending message approval identified by this message id.
    Decide(String, ApprovalAction),
    /// Pop a full detail view (from, to, body) of the pending message.
    ShowDetails(String),
    /// Decide the pending harness registration for this label: approve
    /// (register the shown launch set) or deny. A separate consent from a
    /// message approval (never bundled — security design H3).
    DecideRegistration(String, bool),
    /// Open the attention board (`kitty -e corral`).
    OpenBoard,
    /// Stop the daemon.
    Quit,
}

/// The pending message approval shown in the tray, if any.
struct TrayPending {
    id: String,
    summary: String,
}

/// The pending harness registration shown in the tray, if any: the kind label
/// and a display of the launch set the operator is being asked to trust.
struct TrayRegistration {
    label: String,
    summary: String,
}

/// The ksni tray model. Holds the current pending approval and a channel back
/// to the main loop; every menu activation just sends a `TrayCommand`.
pub struct CorralTray {
    tx: Sender<TrayCommand>,
    pending: Option<TrayPending>,
    pending_reg: Option<TrayRegistration>,
}

impl CorralTray {
    /// Report a decision on the current pending message, then clear it (the
    /// main loop applies it authoritatively; clearing here keeps the menu from
    /// offering a stale second decision).
    fn decide(&mut self, action: ApprovalAction) {
        if let Some(p) = &self.pending {
            let _ = self.tx.send(TrayCommand::Decide(p.id.clone(), action));
        }
        self.pending = None;
    }

    /// Report a decision on the current pending registration, then clear it.
    fn decide_registration(&mut self, approve: bool) {
        if let Some(r) = &self.pending_reg {
            let _ = self.tx.send(TrayCommand::DecideRegistration(r.label.clone(), approve));
        }
        self.pending_reg = None;
    }
}

impl ksni::Tray for CorralTray {
    fn id(&self) -> String {
        "corrald".into()
    }

    fn title(&self) -> String {
        match (&self.pending, &self.pending_reg) {
            (Some(_), _) => "corral — message waiting".into(),
            (_, Some(_)) => "corral — harness to verify".into(),
            _ => "corral".into(),
        }
    }

    // The corral "pen" mark, drawn in code. Calm (gray) normally; the tray
    // switches to the attention pixmap (warm red) when `status` is
    // NeedsAttention, i.e. a message is waiting.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        crate::icon::pen_pixmaps(false)
    }

    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        crate::icon::pen_pixmaps(true)
    }

    // Always `Active` when idle (never `Passive`): the tray is the daemon's
    // always-present control surface, and hosts (Waybar, some KDE/GNOME
    // implementations) hide `Passive` items, which would make corrald
    // invisible until a message happened to arrive.
    fn status(&self) -> ksni::Status {
        if self.pending.is_some() || self.pending_reg.is_some() {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    // Left click on the tray icon (`Activate` over DBus) — right click shows
    // `menu()` instead. There is no pending-approval state worth surfacing
    // here (the menu already covers that), so left click's one job is the
    // fast path: open the board.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayCommand::OpenBoard);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        match &self.pending {
            Some(p) => {
                items.push(
                    StandardItem {
                        label: format!("Pending: {}", p.summary),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
                items.push(
                    StandardItem {
                        label: "Show details…".into(),
                        activate: Box::new(|t: &mut Self| {
                            if let Some(p) = &t.pending {
                                let _ = t.tx.send(TrayCommand::ShowDetails(p.id.clone()));
                            }
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
                items.push(
                    StandardItem {
                        label: "Allow once".into(),
                        activate: Box::new(|t: &mut Self| t.decide(ApprovalAction::AllowOnce)),
                        ..Default::default()
                    }
                    .into(),
                );
                items.push(
                    StandardItem {
                        label: "Allow always".into(),
                        activate: Box::new(|t: &mut Self| t.decide(ApprovalAction::AllowAlways)),
                        ..Default::default()
                    }
                    .into(),
                );
                items.push(
                    StandardItem {
                        label: "Deny".into(),
                        activate: Box::new(|t: &mut Self| t.decide(ApprovalAction::Deny)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            None => items.push(
                StandardItem {
                    label: "No messages waiting".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ),
        }
        // A pending harness registration is a separate section with its own
        // Approve/Deny, never merged into a message approval (security H3).
        items.push(MenuItem::Separator);
        match &self.pending_reg {
            Some(r) => {
                items.push(
                    StandardItem {
                        label: format!("Verify harness {}: {}", r.label, r.summary),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
                items.push(
                    StandardItem {
                        label: "Approve harness".into(),
                        activate: Box::new(|t: &mut Self| t.decide_registration(true)),
                        ..Default::default()
                    }
                    .into(),
                );
                items.push(
                    StandardItem {
                        label: "Deny harness".into(),
                        activate: Box::new(|t: &mut Self| t.decide_registration(false)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            None => items.push(
                StandardItem {
                    label: "No harness to verify".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ),
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open board".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayCommand::OpenBoard);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit corrald".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// A handle the main loop uses to push pending-approval state to the tray and
/// receive operator actions. If the tray could not start (no StatusNotifierHost)
/// the handle is inert and `commands` never yields, so the daemon degrades to
/// notification-only approval with no other change.
pub struct Tray {
    handle: Option<ksni::blocking::Handle<CorralTray>>,
    pub commands: Receiver<TrayCommand>,
}

impl Tray {
    /// Start the tray on its own thread (best-effort).
    pub fn start() -> Self {
        let (tx, commands) = mpsc::channel();
        let handle = match (CorralTray {
            tx,
            pending: None,
            pending_reg: None,
        })
        .spawn()
        {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("corrald: tray unavailable ({e}); using notifications only");
                None
            }
        };
        Tray { handle, commands }
    }

    /// Reflect the current pending message approval (or its absence).
    pub fn set_pending(&self, pending: Option<(String, String)>) {
        if let Some(h) = &self.handle {
            h.update(|t| {
                t.pending = pending.map(|(id, summary)| TrayPending { id, summary });
            });
        }
    }

    /// Reflect the current pending harness registration (or its absence).
    pub fn set_pending_registration(&self, reg: Option<(String, String)>) {
        if let Some(h) = &self.handle {
            h.update(|t| {
                t.pending_reg = reg.map(|(label, summary)| TrayRegistration { label, summary });
            });
        }
    }
}

/// Open the desktop attention board (`corral-gui`), detached (`setsid --fork`)
/// so it outlives the daemon and is not a child of it. The GUI is a native
/// window, so no terminal is needed (unlike the TUI `kitty -e corral`).
pub fn open_board() {
    let _ = Command::new("setsid")
        .arg("--fork")
        .arg(gui_binary())
        .status();
}

/// Resolve the `corral-gui` binary: prefer one sitting beside `corrald` (the
/// same cargo target or install dir), so the tray works without `corral-gui`
/// being on `$PATH`; fall back to the bare name for a PATH lookup.
fn gui_binary() -> std::ffi::OsString {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|d| d.join("corral-gui")) {
            if sibling.is_file() {
                return sibling.into_os_string();
            }
        }
    }
    "corral-gui".into()
}
