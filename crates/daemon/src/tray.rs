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
    /// Decide the pending approval identified by this message id.
    Decide(String, ApprovalAction),
    /// Open the attention board (`kitty -e corral`).
    OpenBoard,
    /// Stop the daemon.
    Quit,
}

/// The pending approval shown in the tray, if any.
struct TrayPending {
    id: String,
    summary: String,
}

/// The ksni tray model. Holds the current pending approval and a channel back
/// to the main loop; every menu activation just sends a `TrayCommand`.
pub struct CorralTray {
    tx: Sender<TrayCommand>,
    pending: Option<TrayPending>,
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
}

impl ksni::Tray for CorralTray {
    fn id(&self) -> String {
        "corrald".into()
    }

    fn title(&self) -> String {
        match &self.pending {
            Some(_) => "corral — message waiting".into(),
            None => "corral".into(),
        }
    }

    // A waiting approval flips the icon to an attention/unread glyph so the
    // tray reads at a glance without opening the menu.
    fn icon_name(&self) -> String {
        match &self.pending {
            Some(_) => "mail-unread-new".into(),
            None => "mail-read".into(),
        }
    }

    fn status(&self) -> ksni::Status {
        match &self.pending {
            Some(_) => ksni::Status::NeedsAttention,
            None => ksni::Status::Passive,
        }
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
        let handle = match (CorralTray { tx, pending: None }).spawn() {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("corrald: tray unavailable ({e}); using notifications only");
                None
            }
        };
        Tray { handle, commands }
    }

    /// Reflect the current pending approval (or its absence) into the tray.
    pub fn set_pending(&self, pending: Option<(String, String)>) {
        if let Some(h) = &self.handle {
            h.update(|t| {
                t.pending = pending.map(|(id, summary)| TrayPending { id, summary });
            });
        }
    }
}

/// Open the attention board in its own detached kitty window. Detached
/// (`setsid --fork`) so it outlives the daemon and is not a child of it.
pub fn open_board() {
    let _ = Command::new("setsid")
        .arg("--fork")
        .arg("kitty")
        .args(["-e", "corral"])
        .status();
}
