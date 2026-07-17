//! corrald binary entry point: a thin shell over `corral_daemon::run`. All
//! security-critical logic lives in the library crate so the test suite can
//! exercise the trust boundary directly (see `docs/security-test-matrix.md`).

fn main() {
    corral_daemon::run();
}
