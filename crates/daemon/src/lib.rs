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

/// Safety net for changes that arrive without an event: an agent appearing or
/// going dormant only writes its registry file, which nothing notifies us
/// about. Everything else (socket message, tray click, notification button)
/// wakes the loop immediately through the event channel, so this may be slow.
///
/// It used to be a flat 200ms poll, which cost 8.6% of a core at idle (0.15W,
/// measured with powertop) because every tick re-scanned and re-parsed the
/// whole registry twice whether or not anything had changed. Blocking on the
/// event channel removes that and delivers messages sooner, not later.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// Everything that can wake the loop. std has no channel select, so the three
/// producer channels are funnelled into one by `relay`, and the loop blocks on
/// that single receiver.
enum Event {
    /// A message submitted over the control socket. Boxed: it dwarfs the other
    /// variants, which would otherwise pay for its size on every send.
    Submitted(Box<mailbox::Submission>),
    /// An approval decision from a desktop notification's buttons.
    Decision(String, ApprovalAction),
    /// A tray menu action.
    Tray(TrayCommand),
}

/// Forward one producer channel into the single event channel. A relay thread
/// parked in `recv` costs nothing (no timer, no wakeup), unlike the poll it
/// replaces; it ends when its producer hangs up.
fn relay<T: Send + 'static>(
    rx: mpsc::Receiver<T>,
    tx: mpsc::Sender<Event>,
    wrap: impl Fn(T) -> Event + Send + 'static,
) {
    std::thread::spawn(move || {
        for item in rx {
            if tx.send(wrap(item)).is_err() {
                break; // loop gone; nothing left to wake
            }
        }
    });
}

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
    let (tray_tx, tray_rx) = mpsc::channel::<TrayCommand>();
    let tray = Tray::start(tray_tx);

    // The one channel the loop blocks on; every producer feeds it.
    let (ev_tx, ev_rx) = mpsc::channel::<Event>();
    relay(msg_rx, ev_tx.clone(), |m| Event::Submitted(Box::new(m)));
    relay(napp_rx, ev_tx.clone(), |(id, action)| {
        Event::Decision(id, action)
    });
    relay(tray_rx, ev_tx, Event::Tray);
    // Harness-registration approvals: the peer of the router's message
    // approvals (separate consent, separate store — H3).
    let mut registrar = Registrar::new(approved_commands_file.clone());
    // Which pending message ids / registration label already have a surface
    // shown, so each fires (and is audited) once. A set, not one slot, so every
    // pending message gets its own notification — no approval hides behind
    // another (paired with the router's multi-pending queue).
    let mut announced: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tray_shown: Option<String> = None;
    let mut announced_reg: Option<String> = None;

    loop {
        // Block until something actually happens, then drain the whole burst.
        // On the timeout tick nothing arrives and only the periodic work below
        // runs, which is what notices agents appearing or going dormant.
        let woke = match ev_rx.recv_timeout(IDLE_TICK) {
            Ok(ev) => Some(ev),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            // Every producer hung up: nothing can ever wake us again.
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        for ev in woke.into_iter().chain(ev_rx.try_iter()) {
            match ev {
                Event::Submitted(m) => router.enqueue(*m),
                // Both surfaces are guarded on the pending id inside
                // apply_decision, so a stale click cannot decide the wrong
                // message.
                Event::Decision(id, action) | Event::Tray(TrayCommand::Decide(id, action)) => {
                    apply_decision(&mut router, &id, action, &audit_log)
                }
                Event::Tray(TrayCommand::DecideRegistration(label, approve)) => {
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
                    announced_reg = None; // re-evaluate what to surface below
                }
                Event::Tray(TrayCommand::ShowDetails(id)) => {
                    if let Some(p) = router.pending_by_id(&id) {
                        notify::show_detail(
                            p.sub.from_cwd.clone(),
                            p.sub.target_label(),
                            p.sub.body().to_string(),
                        );
                    }
                }
                Event::Tray(TrayCommand::OpenBoard) => tray::open_board(),
                Event::Tray(TrayCommand::Quit) => {
                    eprintln!("corrald: quit");
                    return;
                }
            }
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

        // Surface every pending approval: one desktop notification per message
        // (fired once, tracked by id), and the first on the tray (it shows one
        // at a time). Because each notification carries its own id and the
        // router resolves by id, approvals never block on ordering.
        for p in router.pending_messages() {
            let sub = &p.sub;
            if announced.insert(sub.id.clone()) {
                notifier.notify(
                    sub.id.clone(),
                    mailbox::basename(&sub.from_cwd),
                    &sub.target_label_short(),
                    &sub.kind,
                    napp_tx.clone(),
                );
            }
        }
        // The tray reflects the first pending message (or clears when none),
        // updating only when that head changes.
        let head = router.pending().map(|p| {
            let sub = &p.sub;
            let from = mailbox::basename(&sub.from_cwd);
            // The verb prefixes the tray label, so a kill or a new agent is
            // never mistaken for a plain message.
            let verb = match sub.kind {
                mailbox::Kind::Stop { .. } => "stop ",
                mailbox::Kind::Spawn { .. } => "spawn in ",
                mailbox::Kind::Message { .. } => "",
            };
            (
                sub.id.clone(),
                format!("{from} → {verb}{}", sub.target_label_short()),
            )
        });
        if head.as_ref().map(|(id, _)| id) != tray_shown.as_ref() {
            tray.set_pending(head.clone());
            tray_shown = head.map(|(id, _)| id);
        }
        // Forget ids no longer pending, so a later re-submission re-notifies.
        let live: std::collections::HashSet<String> = router
            .pending_messages()
            .map(|p| p.sub.id.clone())
            .collect();
        announced.retain(|id| live.contains(id));
    }
}

/// Apply an approval decision to the pending message named by `id`, and record
/// it in the audit trail (who -> whom, allow/deny). An id that is no longer
/// pending (already resolved, or a stale click on an old notification) is a
/// harmless no-op, so a late decision never disturbs another message.
fn apply_decision(
    router: &mut Router,
    id: &str,
    action: ApprovalAction,
    audit_log: &std::path::Path,
) {
    let Some(line) = router.pending_by_id(id).map(|p| {
        format!(
            "message {action:?}: {} -> {}",
            p.sub.from_cwd,
            p.sub.target_label()
        )
    }) else {
        return;
    };
    if let Err(e) = router.apply(id, action) {
        eprintln!("corrald: whitelist: {e}");
    } else {
        curator::audit(audit_log, &line);
    }
}
