//! agentwrap: run an ACP-mode coding agent and expose its stdio on a unix
//! socket at $XDG_RUNTIME_DIR/acp/<label>-<pid>.sock so managers and other
//! ACP clients can discover and drive it.
//!
//! The wrapper is protocol-agnostic: it pumps raw bytes between the socket
//! and the child's stdin/stdout. One client at a time; the child survives
//! client disconnects, so a manager can reconnect at any time.

use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

mod naming;
use naming::{derive_label, socket_path};

struct Config {
    label: Option<String>,
    command: Vec<String>,
}

fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut label = None;
    let mut rest = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--name" | "-n" => {
                label = Some(
                    it.next()
                        .ok_or_else(|| "--name requires a value".to_string())?
                        .clone(),
                );
            }
            "--" => {
                rest.extend(it.cloned());
                break;
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            _ => {
                // First non-flag argument starts the command.
                rest.push(arg.clone());
                rest.extend(it.cloned());
                break;
            }
        }
    }
    if rest.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(Config {
        label,
        command: rest,
    })
}

const USAGE: &str = "usage: agentwrap [--name <label>] [--] <command> [args...]\n\
Runs <command> (an ACP-mode agent speaking JSON-RPC on stdio) and exposes\n\
its stdio on $XDG_RUNTIME_DIR/acp/<label>-<pid>.sock";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match parse_args(&args) {
        Ok(cfg) => cfg,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let runtime_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            // Fail fast: without a per-user runtime dir there is no safe
            // world-invisible place for the socket.
            eprintln!("agentwrap: XDG_RUNTIME_DIR is not set; refusing to guess a socket location");
            std::process::exit(1);
        }
    };

    let label = derive_label(cfg.label.as_deref(), &cfg.command[0]);
    let sock = socket_path(&runtime_dir, &label, std::process::id());

    if let Err(e) = ensure_private_dir(sock.parent().expect("socket path has a parent")) {
        eprintln!("agentwrap: cannot create socket directory: {e}");
        std::process::exit(1);
    }

    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("agentwrap: cannot bind {}: {e}", sock.display());
            std::process::exit(1);
        }
    };

    let mut child = match Command::new(&cfg.command[0])
        .args(&cfg.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // stderr passes through so harness diagnostics stay visible in the
        // launching terminal.
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&sock);
            eprintln!("agentwrap: cannot spawn {:?}: {e}", cfg.command[0]);
            std::process::exit(1);
        }
    };

    eprintln!("agentwrap: {} -> {}", cfg.command.join(" "), sock.display());

    let child_pid = child.id() as i32;
    let child_stdin = Arc::new(Mutex::new(
        child.stdin.take().expect("child stdin is piped"),
    ));
    let child_stdout = child.stdout.take().expect("child stdout is piped");

    // Forward SIGINT/SIGTERM to the child; its exit then triggers cleanup in
    // the waiter below. Signal safety: we only forward, never clean up here.
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        unsafe {
            signal_hook::low_level::register(sig, move || {
                libc::kill(child_pid, libc::SIGTERM);
            })
        }
        .expect("registering signal handler");
    }

    // Current client connection shared between the accept loop (writer of
    // the slot) and the child-stdout pump (reader of the slot).
    let client = Arc::new(ClientSlot::new());

    // Pump child stdout -> connected client. Runs for the whole child
    // lifetime; while no client is connected it waits (the pipe buffer
    // provides backpressure to the child).
    {
        let client = Arc::clone(&client);
        std::thread::spawn(move || pump_child_stdout(child_stdout, &client));
    }

    // Waiter: when the child exits, unlink the socket and exit with its
    // status. This is the single cleanup point for the normal, signal and
    // client-connected cases alike.
    let waiter = {
        let sock = sock.clone();
        std::thread::spawn(move || {
            let status = child.wait().expect("waiting for child");
            let _ = std::fs::remove_file(&sock);
            std::process::exit(status.code().unwrap_or(1));
        })
    };

    // Accept loop: one client at a time. A second connection while one is
    // active is accepted and immediately closed, which a prober can
    // interpret as "busy".
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if client.is_connected() {
            drop(stream);
            continue;
        }
        let reader = stream.try_clone().expect("cloning unix stream");
        client.connect(stream);
        // Pump client -> child stdin on its own thread so this loop stays
        // free to reject concurrent connection attempts immediately.
        let client = Arc::clone(&client);
        let child_stdin = Arc::clone(&child_stdin);
        std::thread::spawn(move || {
            pump_client_to_child(reader, &child_stdin);
            client.disconnect();
        });
    }

    // Unreachable in practice (incoming() never ends); keep the waiter alive.
    let _ = waiter.join();
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Holds the currently connected client. Connecting/disconnecting notifies
/// the child-stdout pump which blocks while no client is present.
struct ClientSlot {
    stream: Mutex<Option<UnixStream>>,
    changed: Condvar,
    connected: AtomicBool,
}

impl ClientSlot {
    fn new() -> Self {
        Self {
            stream: Mutex::new(None),
            changed: Condvar::new(),
            connected: AtomicBool::new(false),
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn connect(&self, stream: UnixStream) {
        let mut slot = self.stream.lock().unwrap();
        *slot = Some(stream);
        self.connected.store(true, Ordering::SeqCst);
        self.changed.notify_all();
    }

    fn disconnect(&self) {
        let mut slot = self.stream.lock().unwrap();
        *slot = None;
        self.connected.store(false, Ordering::SeqCst);
        self.changed.notify_all();
    }

    /// Blocks until a client is connected, then returns a clone of its stream.
    fn wait_for_client(&self) -> UnixStream {
        let mut slot = self.stream.lock().unwrap();
        loop {
            if let Some(stream) = slot.as_ref() {
                return stream.try_clone().expect("cloning unix stream");
            }
            slot = self.changed.wait(slot).unwrap();
        }
    }
}

/// Reads the child's stdout forever, delivering to whichever client is
/// connected. A chunk that fails to send (client vanished mid-write) is kept
/// and delivered to the next client so no ACP message is silently dropped.
fn pump_child_stdout(mut stdout: impl Read, client: &ClientSlot) {
    let mut buf = [0u8; 8192];
    let mut pending: Option<Vec<u8>> = None;
    loop {
        let chunk: Vec<u8> = match pending.take() {
            Some(chunk) => chunk,
            None => match stdout.read(&mut buf) {
                Ok(0) | Err(_) => return, // child stdout closed; waiter handles exit
                Ok(n) => buf[..n].to_vec(),
            },
        };
        let mut stream = client.wait_for_client();
        if stream
            .write_all(&chunk)
            .and_then(|_| stream.flush())
            .is_err()
        {
            pending = Some(chunk);
            client.disconnect();
        }
    }
}

/// Reads from the client socket and writes to the child's stdin. Returns when
/// the client disconnects. The child's stdin stays open across clients so the
/// agent never sees EOF between manager reconnects.
fn pump_client_to_child(mut stream: UnixStream, child_stdin: &Mutex<impl Write>) {
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut stdin = child_stdin.lock().unwrap();
                if stdin
                    .write_all(&buf[..n])
                    .and_then(|_| stdin.flush())
                    .is_err()
                {
                    return; // child gone; waiter handles exit
                }
            }
        }
    }
}
