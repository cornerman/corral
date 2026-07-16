//! Well-known on-disk locations, shared by the board and the daemon so both
//! agree on where the registry, control socket, and daemon state live. Each is
//! the `env` override if set, else a fixed name under `~/.corral`.
//!
//! The layout is split by trust (security design, Mechanism 2), so the agent
//! sandbox profile is a clean directory rule:
//!
//! ```text
//! ~/.corral/
//!   corrald.sock     # root: agents connect (one file), corrald binds
//!   registry/        # PUBLIC  — agent-writable (session symlinks)
//!   state/           # PRIVATE — daemon-only (whitelist, approved-commands.json)
//! ```
//!
//! The agent sandbox grants write to `registry/` and connect to `corrald.sock`
//! only; `state/` is never on the allowlist, so a compromised agent cannot
//! tamper with the whitelist or pre-register a command. corral cannot enforce
//! the profile (that is deployment glue); these paths just keep every binary
//! agreeing on the boundary.

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

/// The **raw dir-index** file (`~/.corral/registry`): a newline-delimited list
/// of directories where agents have run. Agent-appendable; read only by
/// corrald's curator, which then scans each `<D>/.corral/registry/`.
pub fn registry_index_file() -> Option<PathBuf> {
    corral_path("CORRAL_REGISTRY_INDEX", "registry")
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
