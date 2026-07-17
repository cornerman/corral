//! Well-known on-disk locations, shared by the board and the daemon so both
//! agree on where the registry, control socket, and daemon state live. Each is
//! the `env` override if set, else a fixed name under `~/.corral`.
//!
//! The layout is split by trust (security design, Mechanism 2), so the agent
//! sandbox profile is a clean directory rule:
//!
//! ```text
//! ~/.corral/
//!   corrald.sock       # root: agents connect (one file), corrald binds
//!   input/             # PUBLIC  — agent-writable (write-only)
//!     registry/<id>    #   one per-session POINTER file, content = its cwd
//!   state/             # PRIVATE — daemon-only (whitelist, approved-commands, vetted registry)
//! ```
//!
//! The agent sandbox grants write to `input/` and connect to `corrald.sock`
//! only (`filesystem.write` on the dir, write-only: an agent creates/overwrites
//! its own pointer but cannot read the dir, so it cannot enumerate peers' cwds).
//! `state/` is never on the allowlist, so a compromised agent cannot tamper
//! with the whitelist or pre-register a command, and `corrald.sock` sits at the
//! root of `~/.corral` (not inside the writable dir), so it cannot be rebound.
//! corral cannot enforce the profile (that is deployment glue); these paths
//! just keep every binary agreeing on the boundary.

use std::path::PathBuf;

/// A corral path: the `env` override if set, else `$HOME/.corral/<name>`.
/// `None` only when neither is available. All of corral's on-disk locations
/// share this shape (a well-known name under `~/.corral`, overridable for
/// tests and non-standard setups).
pub fn corral_path(env: &str, name: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(env) {
        return Some(PathBuf::from(v));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corral").join(name))
}

/// The agent-writable input root (`~/.corral/input`): the ONE directory the
/// sandbox grants an agent write on. Holds only untrusted agent-authored data
/// (today the pointer store below), kept apart from the sealed `state/` and the
/// root control socket so the grant is a clean, safe directory rule.
pub fn input_dir() -> Option<PathBuf> {
    corral_path("CORRAL_INPUT_DIR", "input")
}

/// The **raw pointer store** (`~/.corral/input/registry/`): one file per
/// announcing session, named `<sessionId>`, whose content is the cwd it runs
/// in. Agent-written (create/overwrite own file, write-only); read only by
/// corrald's curator, which scans each pointed-at `<cwd>/.corral/registry/`.
/// Mirrors `state/registry/`: raw pointers here, vetted records there.
pub fn input_registry_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("CORRAL_INPUT_REGISTRY") {
        return Some(PathBuf::from(v));
    }
    input_dir().map(|d| d.join("registry"))
}

/// The **vetted registry** directory (`~/.corral/state/registry/`): the
/// authenticated, field-validated, registered records corrald writes and the
/// viewers read. Sealed (under `state/`), so viewers only ever render trusted
/// data.
pub fn state_registry_dir() -> Option<PathBuf> {
    state_path("CORRAL_STATE_REGISTRY", "registry")
}

/// The append-only security audit log (`~/.corral/state/audit.log`): corrald's
/// record of registrations, authorizations, stops, deliveries, and quarantine
/// drops. Sealed under `state/`.
pub fn audit_log() -> Option<PathBuf> {
    state_path("CORRAL_AUDIT_LOG", "audit.log")
}

/// The control socket the `corral_message_agent` tool submits to, and the
/// daemon binds. At the root of `~/.corral` (not inside an agent-writable
/// directory), so a compromised agent gets a single connect capability to the
/// file but cannot unlink and rebind it.
pub fn control_socket() -> Option<PathBuf> {
    corral_path("CORRAL_CONTROL_SOCKET", "corrald.sock")
}

/// The daemon-only state directory (`~/.corral/state`): the whitelist and the
/// approved-command store. Never on the agent sandbox allowlist, so its
/// contents are unwritable by any agent by construction.
pub fn state_dir() -> Option<PathBuf> {
    corral_path("CORRAL_STATE_DIR", "state")
}

/// A file inside the daemon-only `state/` directory, honoring an explicit
/// per-file override first, then `CORRAL_STATE_DIR`, then the default.
fn state_path(file_env: &str, name: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(file_env) {
        return Some(PathBuf::from(v));
    }
    state_dir().map(|d| d.join(name))
}

/// The whitelist file of pre-authorized `(sender -> target)` dir pairs. In the
/// sealed `state/` directory so an agent cannot append its own allow pair.
pub fn whitelist_file() -> Option<PathBuf> {
    state_path("CORRAL_WHITELIST", "whitelist")
}

/// The approved-command store (harness registration, security design T4). In
/// the sealed `state/` directory: corrald is the only writer, agents cannot
/// pre-register a command.
pub fn approved_commands_file() -> Option<PathBuf> {
    state_path("CORRAL_APPROVED_COMMANDS", "approved-commands.json")
}
