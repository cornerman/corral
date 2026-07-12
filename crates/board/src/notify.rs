//! Desktop notification for a pending inter-agent message approval, so the
//! operator can allow/deny from the notification without switching to the
//! corral window. Best-effort and non-blocking: if `notify-send` (with action
//! support) is missing or the daemon ignores actions, nothing happens and the
//! in-board approval dialog still works.

use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread;

use crate::ui::ApprovalAction;

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

/// Map a notify-send action name to a decision. Pure, so it is unit-tested.
fn parse_action(name: &str) -> Option<ApprovalAction> {
    match name.trim() {
        "once" => Some(ApprovalAction::AllowOnce),
        "always" => Some(ApprovalAction::AllowAlways),
        "deny" => Some(ApprovalAction::Deny),
        _ => None, // dismissed, or no action support
    }
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
        let body = format!("from {from}\nto {target}\n\n{message}");
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
    fn parses_action_names() {
        assert_eq!(parse_action("once\n"), Some(ApprovalAction::AllowOnce));
        assert_eq!(parse_action("always"), Some(ApprovalAction::AllowAlways));
        assert_eq!(parse_action(" deny "), Some(ApprovalAction::Deny));
        assert_eq!(parse_action(""), None);
        assert_eq!(parse_action("bogus"), None);
    }
}
