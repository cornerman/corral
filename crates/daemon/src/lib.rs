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
//!
//! The security-critical logic lives in these modules (exposed so the security
//! test suite can exercise the trust boundary directly, see
//! `docs/security-test-matrix.md`); `main.rs` is a thin shell over `run`.

use std::sync::mpsc;
use std::time::Duration;

use corral_core::discovery;
use corral_core::launch::TerminalLauncher;
use corral_core::paths;

pub mod control;
pub mod curator;
pub mod icon;
pub mod mailbox;
pub mod notify;
pub mod registrations;
pub mod router;
pub mod tray;

use crate::notify::{ApprovalNotifier, NotifySendNotifier};
use crate::registrations::Registrar;
use crate::router::{ApprovalAction, Router};
use crate::tray::{Tray, TrayCommand};

/// How often the loop routes queued messages and reflects pending state. A
/// message accepted over the socket is delivered within this window.
const TICK: Duration = Duration::from_millis(200);

/// Run the daemon: bind the control socket (refusing if another instance owns
/// it), then loop — curate the registry, route authorized messages, and
/// reflect pending approvals to the tray + notification. Never returns except
/// on an operator Quit or a fatal bind error (which exits the process).
pub fn run() {
    let (
        Some(pointer_dir),
        Some(state_registry),
        Some(approved_commands_file),
        Some(audit_log),
        Some(socket),
        Some(whitelist),
    ) = (
        paths::input_registry_dir(),
        paths::state_registry_dir(),
        paths::approved_commands_file(),
        paths::audit_log(),
        paths::control_socket(),
        paths::whitelist_file(),
    )
    else {
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

    // Pre-create the agent-writable pointer store before agents launch, so the
    // sandbox's directory grant binds a live inode (fail loud: without it,
    // agents silently cannot announce).
    if let Err(e) = curator::ensure_input(&pointer_dir) {
        eprintln!(
            "corrald: cannot create pointer store {}: {e}",
            pointer_dir.display()
        );
        std::process::exit(1);
    }

    let (msg_tx, msg_rx) = mpsc::channel();
    // Recipient resolution reads the VETTED registry corrald itself curates,
    // never agent-writable records.
    if let Err(e) = control::serve(
        socket.clone(),
        state_registry.clone(),
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

    let launcher = TerminalLauncher;
    let mut router = Router::new(whitelist);
    let notifier = NotifySendNotifier;
    // Decisions from the desktop notification's buttons, tagged with the
    // message id so a stale reply is ignored.
    let (napp_tx, napp_rx) = mpsc::channel::<(String, ApprovalAction)>();
    let tray = Tray::start();
    // Harness-registration approvals: the peer of the router's message
    // approvals (separate consent, separate store — H3).
    let mut registrar = Registrar::new(approved_commands_file.clone());
    // Which pending message id / registration label already has a surface
    // shown, so each fires (and is audited) once.
    let mut announced: Option<String> = None;
    let mut announced_reg: Option<String> = None;

    loop {
        // Accept messages submitted over the control socket.
        while let Ok(m) = msg_rx.try_recv() {
            router.enqueue(m);
        }

        // Curate the untrusted raw index into the vetted state/registry the
        // viewers and our own routing read (parse, don't validate). Only
        // registered kinds are published; the rest come back as pending
        // registrations for the operator to verify.
        let pending_regs = curator::refresh(&pointer_dir, &state_registry, &approved_commands_file);
        registrar.observe(pending_regs);
        // Surface a newly pending registration to the tray (once), and audit it.
        match registrar.current() {
            Some((label, template)) if announced_reg.as_deref() != Some(label) => {
                let desc = curator::describe(template);
                tray.set_pending_registration(Some((label.clone(), desc.clone())));
                curator::audit(
                    &audit_log,
                    &format!("registration pending: {label} [{desc}]"),
                );
                announced_reg = Some(label.clone());
            }
            None if announced_reg.is_some() => {
                tray.set_pending_registration(None);
                announced_reg = None;
            }
            _ => {}
        }
        // Route whatever is authorized; the vetted registry is the daemon's
        // whole view of who is live (socket set) and dormant (socket cleared).
        let entries = discovery::scan_registry(&state_registry);
        if let Some(status) = router.poll(&entries, &launcher) {
            eprintln!("corrald: {status}");
            // Deliveries, spawns, and stops go in the audit trail.
            curator::audit(&audit_log, &status);
        }

        // Reflect a newly pending approval to the tray and a notification (once).
        match router.pending() {
            Some(msg) if announced.as_deref() != Some(&msg.id) => {
                let from = mailbox::basename(&msg.from_cwd);
                // A stop is destructive, so the operator must see it is a kill,
                // not a message: the verb prefixes both surfaces.
                let verb = match msg.action {
                    mailbox::Action::Stop => "stop ",
                    mailbox::Action::Deliver => "",
                };
                tray.set_pending(Some((
                    msg.id.clone(),
                    format!("{from} → {verb}{}", msg.target_label_short()),
                )));
                notifier.notify(
                    msg.id.clone(),
                    &msg.from_cwd,
                    &msg.target_label(),
                    &msg.message,
                    msg.action,
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
                TrayCommand::Decide(id, action) => {
                    apply_decision(&mut router, &id, action, &audit_log)
                }
                TrayCommand::DecideRegistration(label, approve) => {
                    if approve {
                        match registrar.approve(&label) {
                            Ok(true) => curator::audit(&audit_log, &format!("registered: {label}")),
                            Ok(false) => {} // stale click; nothing pending
                            Err(e) => eprintln!("corrald: register {label}: {e}"),
                        }
                    } else {
                        registrar.deny(&label);
                        curator::audit(&audit_log, &format!("registration denied: {label}"));
                    }
                    announced_reg = None; // re-evaluate what to surface next tick
                }
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
            apply_decision(&mut router, &id, action, &audit_log);
        }

        std::thread::sleep(TICK);
    }
}

/// Apply an approval decision only if it still matches the pending message,
/// and record it in the audit trail (who -> whom, allow/deny).
fn apply_decision(
    router: &mut Router,
    id: &str,
    action: ApprovalAction,
    audit_log: &std::path::Path,
) {
    if router.pending().map(|m| m.id.as_str()) == Some(id) {
        let line = router
            .pending()
            .map(|m| format!("message {action:?}: {} -> {}", m.from_cwd, m.target_label()))
            .unwrap_or_default();
        if let Err(e) = router.apply(action) {
            eprintln!("corrald: whitelist: {e}");
        } else {
            curator::audit(audit_log, &line);
        }
    }
}
