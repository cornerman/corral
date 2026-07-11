//! End-to-end test: wrap `cat` (echoes stdio), connect via the announced
//! socket, verify byte round-trip, reconnect survival, and socket cleanup.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct Wrapper {
    child: Child,
    sock: PathBuf,
    _runtime_dir: tempfile::TempDir,
}

impl Drop for Wrapper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_wrapper() -> Wrapper {
    let runtime_dir = tempfile::TempDir::new().expect("tempdir");
    let child = Command::new(env!("CARGO_BIN_EXE_agentwrap"))
        .args(["--name", "echo", "--", "cat"])
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .spawn()
        .expect("spawning agentwrap");
    let sock = runtime_dir
        .path()
        .join("acp")
        .join(format!("echo-{}.sock", child.id()));
    wait_for(|| sock.exists(), "socket to appear");
    Wrapper {
        child,
        sock,
        _runtime_dir: runtime_dir,
    }
}

fn wait_for(cond: impl Fn() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Connect and echo `msg`. Retries: right after a previous client
/// disconnects, the wrapper may still see it as connected and bounce us
/// (accept-then-close). That window is part of the contract; clients retry.
fn roundtrip(sock: &Path, msg: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = try_roundtrip(sock, msg);
        match result {
            Ok(got) => {
                assert_eq!(got, msg);
                return;
            }
            Err(e) => {
                assert!(Instant::now() < deadline, "roundtrip kept failing: {e}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Connect and prove the connection is the active one (ping echoes),
/// retrying through the post-disconnect bounce window.
fn connect_ready(sock: &Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let attempt = (|| -> std::io::Result<UnixStream> {
            let mut stream = UnixStream::connect(sock)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            stream.write_all(b"ping\n")?;
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf)?;
            assert_eq!(&buf, b"ping\n");
            Ok(stream)
        })();
        match attempt {
            Ok(stream) => return stream,
            Err(e) => {
                assert!(Instant::now() < deadline, "connect kept failing: {e}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn try_roundtrip(sock: &Path, msg: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut stream = UnixStream::connect(sock)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(msg)?;
    let mut got = vec![0u8; msg.len()];
    stream.read_exact(&mut got)?;
    Ok(got)
}

#[test]
fn echo_roundtrip_reconnect_and_cleanup() {
    let mut wrapper = start_wrapper();

    // Round-trip through the wrapped `cat`.
    roundtrip(&wrapper.sock, b"{\"jsonrpc\":\"2.0\",\"id\":0}\n");

    // Child must survive a client disconnect: a second client works.
    roundtrip(&wrapper.sock, b"hello again\n");

    // Second concurrent client is rejected (accept-then-close) while the
    // first stays connected and functional.
    let first = connect_ready(&wrapper.sock);
    let mut second = UnixStream::connect(&wrapper.sock).expect("second connects");
    second
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let n = second.read(&mut [0u8; 1]).expect("second client read");
    assert_eq!(n, 0, "second concurrent client must be closed immediately");
    drop(first);

    // SIGTERM to the wrapper terminates the child and unlinks the socket.
    unsafe { libc::kill(wrapper.child.id() as i32, libc::SIGTERM) };
    let sock = wrapper.sock.clone();
    wait_for(|| !sock.exists(), "socket cleanup after SIGTERM");
    let _ = wrapper.child.wait();
}
