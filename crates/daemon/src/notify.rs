//! Desktop notification for a pending inter-agent message approval, so the
//! operator can allow/deny from the notification without switching to the
//! corral window. Best-effort and non-blocking: if `notify-send` (with action
//! support) is missing or the daemon ignores actions, nothing happens and the
//! in-board approval dialog still works.

use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread;

use crate::router::ApprovalAction;

/// Fire a notification for a pending approval; the chosen action comes back on
/// a channel, tagged with the message id so a stale reply can be ignored.
pub trait ApprovalNotifier {
    fn notify(
        &self,
        id: String,
        from: &str,
        target: &str,
        message: &str,
        tx: Sender<(String, ApprovalAction)>,
    );
}

/// Clip to `n` chars with an ellipsis. Notifications do not scroll, and a long
/// body pushes the action buttons off-screen, so the message is kept short.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// Map a notify-send action name to a decision. Pure, so it is unit-tested.
fn parse_action(name: &str) -> Option<ApprovalAction> {
    match name.trim() {
        "once" => Some(ApprovalAction::AllowOnce),
        "always" => Some(ApprovalAction::AllowAlways),
        "deny" => Some(ApprovalAction::Deny),
        _ => None, // dismissed, or no action support
    }
}

/// Pop a full detail view of a pending message — from, to, and the whole body —
/// as a desktop notification with no action buttons (informational only),
/// triggered from the tray's "Show details". Critical urgency keeps it on
/// screen; best-effort and non-blocking like the approval notification.
pub fn show_detail(from: String, target: String, body: String) {
    thread::spawn(move || {
        let text = format!("from: {from}\nto:   {target}\n\n{body}");
        let _ = Command::new("notify-send")
            .args([
                "-u",
                "critical",
                "-a",
                "corral",
                "corral: message details",
                &text,
            ])
            .output();
    });
}

pub struct NotifySendNotifier;

impl ApprovalNotifier for NotifySendNotifier {
    fn notify(
        &self,
        id: String,
        from: &str,
        target: &str,
        message: &str,
        tx: Sender<(String, ApprovalAction)>,
    ) {
        // Compact: basename the paths, clip the message.
        let from_s = from.rsplit('/').next().unwrap_or(from);
        let to_s = target.rsplit('/').next().unwrap_or(target);
        let body = format!("{from_s} → {to_s}\n{}", clip(message, 140));
        thread::spawn(move || {
            // `-A name=Label` implies --wait and prints the chosen name to
            // stdout; critical urgency keeps it on screen until acted on.
            let out = Command::new("notify-send")
                .args([
                    "-u",
                    "critical",
                    "-a",
                    "corral",
                    "corral: agent message",
                    &body,
                ])
                .args([
                    "-A",
                    "once=Allow once",
                    "-A",
                    "always=Allow always",
                    "-A",
                    "deny=Deny",
                ])
                .output();
            if let Ok(out) = out {
                if let Some(action) = parse_action(&String::from_utf8_lossy(&out.stdout)) {
                    let _ = tx.send((id, action));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_adds_ellipsis_when_too_long() {
        assert_eq!(clip("short", 140), "short");
        assert_eq!(clip("abcdef", 4), "abc…");
    }

    #[test]
    fn parses_action_names() {
        assert_eq!(parse_action("once\n"), Some(ApprovalAction::AllowOnce));
        assert_eq!(parse_action("always"), Some(ApprovalAction::AllowAlways));
        assert_eq!(parse_action(" deny "), Some(ApprovalAction::Deny));
        assert_eq!(parse_action(""), None);
        assert_eq!(parse_action("bogus"), None);
    }
}
