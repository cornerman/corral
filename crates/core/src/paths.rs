//! Well-known on-disk locations, shared by the board and the daemon so both
//! agree on where the registry, control socket, and whitelist live. Each is the
//! `env` override if set, else a fixed name under `~/.corral`.

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

/// The registry directory: the session records both binaries discover.
pub fn registry_dir() -> Option<PathBuf> {
    corral_path("CORRAL_REGISTRY_DIR", "registry")
}

/// The control socket the `corral_message_agent` tool submits to, and the
/// daemon binds. Under `~/.corral`, which is on the agent sandbox allowlist.
pub fn control_socket() -> Option<PathBuf> {
    corral_path("CORRAL_CONTROL_SOCKET", "corrald.sock")
}

/// The whitelist file of pre-authorized `(sender -> target)` dir pairs.
pub fn whitelist_file() -> Option<PathBuf> {
    corral_path("CORRAL_WHITELIST", "whitelist")
}
