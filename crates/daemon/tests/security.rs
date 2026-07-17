//! Layer A of the security suite (see `docs/security-test-matrix.md`):
//! in-process integration tests over a real tmpfs, exercising the trust
//! boundary through corral's own code. Each test names the SECURITY.md threat
//! it assures. Real filesystem semantics (FIFO, symlink, `/proc/self/fd`, mode
//! bits) are the point — a unit test would mock away exactly what is defended.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use corral_core::approved_commands::{self, Approved, Template};
use corral_core::curation;
use corral_core::discovery::{self, RegistryEntry};
use corral_daemon::curator;

// --- helpers ---------------------------------------------------------------

/// Create `<root>/<name>/.corral/registry/` and return the canonical workdir
/// path (canonicalized because curation attributes records to the *real* dir).
fn workdir(root: &Path, name: &str) -> String {
    let cwd = root.join(name);
    fs::create_dir_all(cwd.join(".corral").join("registry")).unwrap();
    fs::canonicalize(&cwd)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Write a record JSON into a workdir's raw (agent-writable) registry.
fn write_record(cwd: &str, sid: &str, fields: &[(&str, serde_json::Value)]) {
    let mut m = serde_json::Map::new();
    m.insert("sessionId".into(), sid.into());
    for (k, v) in fields {
        m.insert((*k).into(), v.clone());
    }
    let path = Path::new(cwd)
        .join(".corral")
        .join("registry")
        .join(format!("{sid}.json"));
    fs::write(
        path,
        serde_json::to_string(&serde_json::Value::Object(m)).unwrap(),
    )
    .unwrap();
}

/// Write the raw pointer store (`input/registry/`) with one file per workdir,
/// content = the workdir path. Returns the pointer dir corrald curates from.
fn write_index(root: &Path, dirs: &[&str]) -> PathBuf {
    let ptrdir = root.join("input/registry");
    fs::create_dir_all(&ptrdir).unwrap();
    for (i, dir) in dirs.iter().enumerate() {
        fs::write(ptrdir.join(format!("p{i}")), dir).unwrap();
    }
    ptrdir
}

/// Persist an approved store registering `label` with the given launch set.
fn approve(path: &Path, label: &str, tmpl: Template) {
    let mut a = Approved::new();
    a.insert(label.into(), tmpl);
    approved_commands::write_approved(path, &a).unwrap();
}

/// The set of sessionIds corrald published to the vetted `state/registry/`.
fn published(state_registry: &Path) -> Vec<String> {
    let mut ids: Vec<String> = discovery::scan_registry(state_registry)
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    ids.sort();
    ids
}

// --- T2 / T14: the outbox submit path --------------------------------------

#[test]
fn t2_cwd_from_location_not_content() {
    // resolve_submission derives fromCwd from where the file physically lives,
    // never from a self-reported directory in the content.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    let outbox = Path::new(&cwd).join(".corral").join("outbox");
    fs::create_dir_all(&outbox).unwrap();
    let msg = outbox.join("m1.json");
    fs::write(&msg, r#"{"fromCwd":"/victim","message":"hi"}"#).unwrap();

    let (derived, content) = curation::resolve_submission(&msg).expect("regular outbox file");
    assert_eq!(
        derived, cwd,
        "cwd is the real location, not the claimed /victim"
    );
    assert!(
        content.contains("/victim"),
        "content returned verbatim for the caller to override"
    );
}

#[test]
fn t2_symlink_target_outside_outbox_rejected() {
    // The identity follows the opened fd's real path (/proc/self/fd), so a
    // symlink at the outbox name pointing elsewhere resolves to its target and
    // fails the outbox-shape check — defeating a post-open redirect (T2).
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    let outbox = Path::new(&cwd).join(".corral").join("outbox");
    fs::create_dir_all(&outbox).unwrap();
    let secret = tmp.path().join("secret.json");
    fs::write(&secret, "{}").unwrap();
    let link = outbox.join("m1.json");
    std::os::unix::fs::symlink(&secret, &link).unwrap();

    assert_eq!(curation::resolve_submission(&link), None);
}

#[test]
fn t14_rejects_fifo() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    let outbox = Path::new(&cwd).join(".corral").join("outbox");
    fs::create_dir_all(&outbox).unwrap();
    let fifo = outbox.join("m1.json");
    let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");

    // Non-blocking open + regular-file check: a FIFO cannot hang or be read.
    assert_eq!(curation::resolve_submission(&fifo), None);
}

#[test]
fn t14_rejects_dir_and_oversize_and_outside_outbox() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    let corral = Path::new(&cwd).join(".corral");
    let outbox = corral.join("outbox");
    fs::create_dir_all(&outbox).unwrap();

    // A directory at the submit path is not a regular file.
    let asdir = outbox.join("d.json");
    fs::create_dir_all(&asdir).unwrap();
    assert_eq!(curation::resolve_submission(&asdir), None, "dir rejected");

    // Oversize (> 256 KiB cap).
    let big = outbox.join("big.json");
    fs::write(&big, vec![b'x'; 256 * 1024 + 1]).unwrap();
    assert_eq!(
        curation::resolve_submission(&big),
        None,
        "oversize rejected"
    );

    // A regular file NOT under .corral/outbox is rejected (wrong shape).
    let stray = corral.join("stray.json");
    fs::write(&stray, "{}").unwrap();
    assert_eq!(
        curation::resolve_submission(&stray),
        None,
        "outside outbox rejected"
    );
}

// --- T3: forged / cross-directory records ----------------------------------

#[test]
fn t3_record_attributed_to_physical_dir_ignoring_content_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    // The record lies about its cwd; curation stamps the physical location.
    write_record(
        &cwd,
        "s1",
        &[("label", "pi".into()), ("cwd", "/victim".into())],
    );
    let idx = write_index(tmp.path(), &[&cwd]);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json");
    approve(&approved, "pi", Template::default()); // no commands -> matches

    curator::refresh(&idx, &state, &approved);

    let recs = discovery::scan_registry(&state);
    assert_eq!(recs.len(), 1);
    assert_eq!(
        recs[0].cwd.as_deref(),
        Some(cwd.as_str()),
        "cwd is physical, not /victim"
    );
}

#[test]
fn t3_bad_session_id_or_filename_mismatch_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    // Filename stem is `x` but the content claims sessionId `y`: vet rejects.
    let path = Path::new(&cwd)
        .join(".corral")
        .join("registry")
        .join("x.json");
    fs::write(&path, r#"{"sessionId":"y","label":"pi"}"#).unwrap();
    let idx = write_index(tmp.path(), &[&cwd]);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json");
    approve(&approved, "pi", Template::default());

    curator::refresh(&idx, &state, &approved);
    assert!(
        published(&state).is_empty(),
        "filename/sessionId mismatch quarantined"
    );
}

// --- T4: harness registration ----------------------------------------------

fn pi_record(cwd: &str, sid: &str) {
    write_record(cwd, sid, &[("label", "pi".into())]);
}

#[test]
fn t4_unregistered_kind_is_quarantined_and_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    pi_record(&cwd, "s1");
    let idx = write_index(tmp.path(), &[&cwd]);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json"); // absent -> empty store

    let pending = curator::refresh(&idx, &state, &approved);

    assert!(
        published(&state).is_empty(),
        "unregistered kind not published"
    );
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, "pi");
}

#[test]
fn t4_registered_kind_publishes_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    pi_record(&cwd, "s1");
    let idx = write_index(tmp.path(), &[&cwd]);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json");
    approve(&approved, "pi", Template::default());

    let pending = curator::refresh(&idx, &state, &approved);

    assert_eq!(published(&state), vec!["s1".to_string()]);
    assert!(
        pending.is_empty(),
        "already-registered kind prompts nothing"
    );
}

#[test]
fn t4_deviating_launch_set_is_a_new_pending_never_published() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    // Registered pi has a plain spawn; the record smuggles a bash spawn.
    write_record(
        &cwd,
        "s1",
        &[
            ("label", "pi".into()),
            (
                "spawnCommand",
                serde_json::json!(["bash", "-c", "rm -rf ~"]),
            ),
        ],
    );
    let idx = write_index(tmp.path(), &[&cwd]);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json");
    approve(
        &approved,
        "pi",
        Template {
            spawn: Some(vec!["pi".into()]),
            ..Default::default()
        },
    );

    let pending = curator::refresh(&idx, &state, &approved);

    assert!(
        published(&state).is_empty(),
        "deviating argv never executed/published"
    );
    assert_eq!(
        pending.len(),
        1,
        "surfaces as a fresh registration to verify"
    );
    assert_eq!(
        pending[0].1.spawn.as_deref(),
        Some(&["bash".to_string(), "-c".into(), "rm -rf ~".into()][..])
    );
}

#[test]
fn t4_flood_of_novel_labels_all_pending_none_published() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs: Vec<String> = (0..6)
        .map(|i| workdir(tmp.path(), &format!("box{i}")))
        .collect();
    for (i, cwd) in dirs.iter().enumerate() {
        write_record(
            cwd,
            &format!("s{i}"),
            &[("label", format!("kind{i}").into())],
        );
    }
    let refs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let idx = write_index(tmp.path(), &refs);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json"); // empty

    let pending = curator::refresh(&idx, &state, &approved);

    assert!(published(&state).is_empty(), "flood publishes nothing");
    assert_eq!(pending.len(), 6, "one review item per distinct novel kind");
}

// --- T16 / T17: field validation on vet ------------------------------------

#[test]
fn t16_vet_rejects_flag_like_session_id() {
    // A sessionId that could be mistaken for a CLI flag never reaches launch.
    let rec = mk_rec("--config=/evil", None);
    assert!(curation::vet("/box", "--config=/evil", rec).is_none());
}

#[test]
fn t17_socket_must_resolve_inside_own_corral() {
    // A record aiming its socket at another box's session is rejected (T17);
    // one inside its own .corral is accepted.
    let foreign = mk_rec("s1", Some("/other/.corral/pi-1.sock"));
    assert!(
        curation::vet("/box", "s1", foreign).is_none(),
        "foreign socket rejected"
    );

    let own = mk_rec("s1", Some("/box/.corral/pi-1.sock"));
    assert!(
        curation::vet("/box", "s1", own).is_some(),
        "own-box socket accepted"
    );
}

fn mk_rec(sid: &str, socket: Option<&str>) -> RegistryEntry {
    RegistryEntry {
        session_id: sid.into(),
        cwd: Some("/lie".into()),
        title: None,
        socket: socket.map(PathBuf::from),
        spawn_command: None,
        resume_command: None,
        label: Some("pi".into()),
        last_seen: None,
        gui: false,
        message_flag: None,
        hidden: false,
        description: None,
    }
}

// --- seal modes -------------------------------------------------------------

#[test]
fn seal_state_registry_created_0700() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    pi_record(&cwd, "s1");
    let idx = write_index(tmp.path(), &[&cwd]);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json");
    approve(&approved, "pi", Template::default());

    curator::refresh(&idx, &state, &approved);

    let mode = fs::metadata(&state).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "vetted registry dir is owner-only");
}

// --- T13: no network surface (by construction) -----------------------------

#[test]
fn t13_no_tcp_listener_in_workspace() {
    // The claim is "no ports": assert no crate ever names a TCP socket type, so
    // a network listener cannot be introduced without this guard failing.
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut offenders = Vec::new();
    fn walk(dir: &Path, offenders: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                // Scan shipped source only, not the test suite (which names the
                // types in its own guard) or build artifacts.
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if matches!(name.as_str(), "tests" | "target" | ".git") {
                    continue;
                }
                walk(&p, offenders);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let src = fs::read_to_string(&p).unwrap_or_default();
                if src.contains("TcpListener") || src.contains("TcpStream") {
                    offenders.push(p.display().to_string());
                }
            }
        }
    }
    walk(crates, &mut offenders);
    assert!(
        offenders.is_empty(),
        "TCP socket types found (network surface): {offenders:?}"
    );
}

// --- viewer sees only vetted data ------------------------------------------

#[test]
fn viewer_renders_only_vetted_records() {
    use corral_core::engine::Engine;
    // An unregistered (quarantined) record exists in a workdir + index, but the
    // viewer reads only state/registry, so it renders nothing.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = workdir(tmp.path(), "box");
    pi_record(&cwd, "s1");
    let idx = write_index(tmp.path(), &[&cwd]);
    let state = tmp.path().join("state").join("registry");
    let approved = tmp.path().join("approved.json"); // empty -> quarantined

    curator::refresh(&idx, &state, &approved);
    fs::create_dir_all(&state).unwrap(); // viewer dir exists even if empty

    let mut engine = Engine::new(state.clone());
    engine.tick();
    let total: usize = engine.board().column_counts().iter().sum();
    assert_eq!(total, 0, "quarantined record never reaches a viewer");
}
