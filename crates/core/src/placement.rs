//! Placement is which side an agent's window lives on: visible on the host, or
//! hidden in a headless cage. A live surface cannot migrate between
//! compositors, so changing placement is always kill-and-resume: stop the
//! current instance, relaunch from the persisted session on the other side.
//! `h` in either shell toggles placement via `placement_for` + `apply_placement`.

use std::path::Path;
use std::process::Command;

use crate::focus::WindowFocuser;
use crate::launch::Launcher;
use crate::model::{Agent, Origin};

/// What `h` does to the selected agent, decided purely from its placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Live + hidden: kill it, resume visible in the foreground.
    Reveal,
    /// Live + visible: close the window, resume hidden in a cage.
    Hide,
    /// Dormant: resume it hidden (start in the background).
    StartHidden,
}

/// Decide the placement toggle for an agent (pure).
pub fn placement_for(origin: Origin, hidden: bool) -> Placement {
    match (origin, hidden) {
        (Origin::Live, true) => Placement::Reveal,
        (Origin::Live, false) => Placement::Hide,
        (Origin::Dormant, _) => Placement::StartHidden,
    }
}

/// Execute the placement toggle for `agent`. Kill-and-resume in every branch:
/// there is no live surface migration between compositors. `kill` terminates a
/// pid (real: `kill_pid`); a stub in tests. Errors if the agent has no cwd or
/// resume command to relaunch from.
pub fn apply_placement(
    agent: &Agent,
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
    kill: &dyn Fn(u32) -> Result<(), String>,
) -> Result<(), String> {
    let placement = placement_for(agent.origin, agent.hidden);
    let cwd = agent
        .cwd
        .as_deref()
        .ok_or("placement: agent has no cwd to relaunch in")?;
    let command = agent
        .resume_argv()
        .ok_or("placement: agent has no resume command")?;
    // Target placement's launch mode: reveal -> visible, hide/start -> hidden.
    let mut mode = agent.launch_mode();
    match placement {
        Placement::Reveal => {
            // Hidden agent has no host window; kill its pid directly. cage
            // exits when its only app does, so the record then goes dormant.
            kill(agent.pid)?;
            mode.hidden = false;
        }
        Placement::Hide => {
            // Visible agent: close its host window (kill the window pid via the
            // focuser), then resume into a headless cage.
            focuser
                .close(agent)
                .map_err(|e| format!("hide close: {e}"))?;
            mode.hidden = true;
        }
        Placement::StartHidden => {
            // Dormant: nothing running to kill; just resume hidden.
            mode.hidden = true;
        }
    }
    launcher
        .launch(Path::new(cwd), &command, None, &mode)
        .map_err(|e| format!("placement resume: {e}"))
}

/// Terminate a process by pid via `kill(1)` (best-effort SIGTERM). The real
/// `kill` passed to `apply_placement`.
pub fn kill_pid(pid: u32) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::WindowFocuser;
    use crate::launch::{LaunchMode, Launcher};
    use crate::model::{Agent, Origin, State};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    fn live_agent(hidden: bool) -> Agent {
        Agent {
            socket_path: PathBuf::from("/tmp/p/.corral/pi-7.sock"),
            pid: 7,
            label: "pi".into(),
            session_id: Some("s1".into()),
            title: None,
            cwd: Some("/tmp/p".into()),
            state: State::Idle,
            origin: Origin::Live,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec!["pi".into(), "--session".into(), "{sessionId}".into()]),
            activity: None,
            gui: false,
            message_flag: None,
            hidden,
            state_since: std::time::Instant::now(),
            last_activity: std::time::Instant::now(),
        }
    }

    #[derive(Default)]
    struct Stub {
        closed: RefCell<Vec<u32>>,
        launched: RefCell<Vec<(Vec<String>, LaunchMode)>>,
        killed: RefCell<Vec<u32>>,
    }
    impl WindowFocuser for Stub {
        fn focus(&self, _a: &Agent) -> Result<(), String> {
            Ok(())
        }
        fn close(&self, a: &Agent) -> Result<(), String> {
            self.closed.borrow_mut().push(a.pid);
            Ok(())
        }
    }
    impl Launcher for Stub {
        fn launch(
            &self,
            _cwd: &Path,
            command: &[String],
            _m: Option<&str>,
            mode: &LaunchMode,
        ) -> Result<(), String> {
            self.launched
                .borrow_mut()
                .push((command.to_vec(), mode.clone()));
            Ok(())
        }
    }

    #[test]
    fn placement_dispatch_is_pure() {
        assert_eq!(placement_for(Origin::Live, true), Placement::Reveal);
        assert_eq!(placement_for(Origin::Live, false), Placement::Hide);
        assert_eq!(
            placement_for(Origin::Dormant, false),
            Placement::StartHidden
        );
        assert_eq!(placement_for(Origin::Dormant, true), Placement::StartHidden);
    }

    #[test]
    fn reveal_kills_pid_then_resumes_visible() {
        let s = Stub::default();
        let a = live_agent(true);
        apply_placement(&a, &s, &s, &|p| {
            s.killed.borrow_mut().push(p);
            Ok(())
        })
        .unwrap();
        assert_eq!(*s.killed.borrow(), vec![7], "reveal kills the agent pid");
        assert!(
            s.closed.borrow().is_empty(),
            "reveal does not use focuser.close"
        );
        let launched = s.launched.borrow();
        assert_eq!(launched.len(), 1);
        assert_eq!(launched[0].0, vec!["pi", "--session", "s1"]);
        assert!(!launched[0].1.hidden, "reveal resumes visible");
    }

    #[test]
    fn hide_closes_window_then_resumes_hidden() {
        let s = Stub::default();
        let a = live_agent(false);
        apply_placement(&a, &s, &s, &|_p| panic!("hide must not kill by pid")).unwrap();
        assert_eq!(*s.closed.borrow(), vec![7], "hide closes the host window");
        let launched = s.launched.borrow();
        assert_eq!(launched.len(), 1);
        assert!(launched[0].1.hidden, "hide resumes hidden");
    }

    #[test]
    fn start_hidden_resumes_dormant_without_kill() {
        let s = Stub::default();
        let mut a = live_agent(false);
        a.origin = Origin::Dormant;
        a.pid = 0;
        apply_placement(&a, &s, &s, &|_p| panic!("dormant has no process to kill")).unwrap();
        assert!(s.closed.borrow().is_empty());
        let launched = s.launched.borrow();
        assert_eq!(launched.len(), 1);
        assert!(launched[0].1.hidden, "start-hidden resumes hidden");
    }

    #[test]
    fn missing_resume_command_is_an_error() {
        let s = Stub::default();
        let mut a = live_agent(true);
        a.resume_command = None;
        assert!(apply_placement(&a, &s, &s, &|_| Ok(())).is_err());
    }
}
