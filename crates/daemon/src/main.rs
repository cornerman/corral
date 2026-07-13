//! corrald: the headless message-routing daemon for corral.
//!
//! One singleton process owns inter-agent messaging: it binds the control
//! socket (`~/.corral/corrald.sock`), authorizes each agent-initiated message
//! against the whitelist, and delivers it — reusing a live agent over its
//! socket, or spawning/resuming one with the message as its first prompt.
//! Unlike the board (`corral`), which is a read-only reflector anyone may launch
//! many times, exactly one corrald may own the control socket, so a second
//! instance refuses to start.
//!
//! The approval gate for a not-yet-whitelisted pair surfaces on the system tray
//! (the reliable path) and as a desktop notification (a convenience mirror).
//! The daemon and the board never talk to each other; they share only the
//! filesystem registry.

use std::sync::mpsc;
use std::time::Duration;

use corral_core::discovery;
use corral_core::launch::KittyLauncher;
use corral_core::paths;

mod control;
mod icon;
mod mailbox;
mod notify;
mod router;
mod tray;

use notify::{ApprovalNotifier, NotifySendNotifier};
use router::{ApprovalAction, Router};
use tray::{Tray, TrayCommand};

/// How often the loop routes queued messages and reflects pending state. A
/// message accepted over the socket is delivered within this window.
const TICK: Duration = Duration::from_millis(200);

fn main() {
    let (Some(registry_dir), Some(socket), Some(whitelist)) = (
        paths::registry_dir(),
        paths::control_socket(),
        paths::whitelist_file(),
    ) else {
        eprintln!("corrald: set $HOME or the CORRAL_* path overrides");
        std::process::exit(1);
    };

    // Singleton guard: one corrald owns the control socket. A live listener
    // means another daemon is already running; refuse rather than hijack it.
    if control::is_serving(&socket) {
        eprintln!(
            "corrald: already running (control socket {} is live)",
            socket.display()
        );
        std::process::exit(1);
    }

    let (msg_tx, msg_rx) = mpsc::channel();
    if let Err(e) = control::serve(
        socket.clone(),
        registry_dir.clone(),
        whitelist.clone(),
        msg_tx,
    ) {
        eprintln!(
            "corrald: cannot bind control socket {}: {e}",
            socket.display()
        );
        std::process::exit(1);
    }
    eprintln!("corrald: serving {}", socket.display());

    let launcher = KittyLauncher;
    let mut router = Router::new(whitelist);
    let notifier = NotifySendNotifier;
    // Decisions from the desktop notification's buttons, tagged with the
    // message id so a stale reply is ignored.
    let (napp_tx, napp_rx) = mpsc::channel::<(String, ApprovalAction)>();
    let tray = Tray::start();
    // Which pending message id already has a tray/notification shown, so each
    // fires once.
    let mut announced: Option<String> = None;

    loop {
        // Accept messages submitted over the control socket.
        while let Ok(m) = msg_rx.try_recv() {
            router.enqueue(m);
        }

        // Route whatever is authorized; a fresh registry scan is the daemon's
        // whole view of who is live (socket set) and dormant (socket cleared).
        let entries = discovery::scan_registry(&registry_dir);
        if let Some(status) = router.poll(&entries, &launcher) {
            eprintln!("corrald: {status}");
        }

        // Reflect a newly pending approval to the tray and a notification (once).
        match router.pending() {
            Some(msg) if announced.as_deref() != Some(&msg.id) => {
                let from = mailbox::basename(&msg.from_cwd);
                tray.set_pending(Some((
                    msg.id.clone(),
                    format!("{from} → {}", msg.target_label_short()),
                )));
                notifier.notify(
                    msg.id.clone(),
                    &msg.from_cwd,
                    &msg.target_label(),
                    &msg.message,
                    napp_tx.clone(),
                );
                announced = Some(msg.id.clone());
            }
            None if announced.is_some() => {
                tray.set_pending(None);
                announced = None;
            }
            _ => {}
        }

        // Apply decisions from the tray and the notification. Both are guarded
        // on the current pending id so a stale click cannot decide the wrong
        // message.
        while let Ok(cmd) = tray.commands.try_recv() {
            match cmd {
                TrayCommand::Decide(id, action) => apply_decision(&mut router, &id, action),
                TrayCommand::ShowDetails(id) => {
                    if let Some(msg) = router.pending().filter(|m| m.id == id) {
                        notify::show_detail(
                            msg.from_cwd.clone(),
                            msg.target_label(),
                            msg.message.clone(),
                        );
                    }
                }
                TrayCommand::OpenBoard => tray::open_board(),
                TrayCommand::Quit => {
                    eprintln!("corrald: quit");
                    return;
                }
            }
        }
        while let Ok((id, action)) = napp_rx.try_recv() {
            apply_decision(&mut router, &id, action);
        }

        std::thread::sleep(TICK);
    }
}

/// Apply an approval decision only if it still matches the pending message.
fn apply_decision(router: &mut Router, id: &str, action: ApprovalAction) {
    if router.pending().map(|m| m.id.as_str()) == Some(id) {
        if let Err(e) = router.apply(action) {
            eprintln!("corrald: whitelist: {e}");
        }
    }
}
