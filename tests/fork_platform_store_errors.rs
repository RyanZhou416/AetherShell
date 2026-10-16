//! The platform store's read and write paths, driven through this build's own
//! binary.
//!
//! `platform_db_path` answers from `HOME`/`USERPROFILE`, so every case here runs
//! in a child whose `HOME` and `USERPROFILE` both point at a private temporary
//! directory. Nothing here reads or writes the invoking user's store, and the
//! parent process environment is never changed: mode, workspace and home are set
//! on the child's own command.
//!
//! The defects these tests pin are data loss. `platform_db_list` and
//! `platform_db_delete` parsed the store with `unwrap_or_default`, so a document
//! that could not be parsed was reported as empty and then written back holding
//! only the new contents. `platform_db_store` and `platform_db_import` read with
//! `unwrap_or_else(|_| "{}")`, so a file that could not be decoded as UTF-8 was
//! replaced by a fresh document. `platform_db_init`, `platform_db_store` and
//! `platform_db_import` ran no guard at all before creating `.aether`, so the
//! write outside the workspace was the one thing that happened.
//!
//! Every assertion about the file reads it back with `std::fs`: a builtin's own
//! report of what it did is not evidence that it did it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};

/// How long one child may take. A refusal that never returns is a hang to
/// whoever runs this file, so the wait is bounded and an overrun is killed.
const BOUND: Duration = Duration::from_secs(10);

// ── the fixture ─────────────────────────────────────────────────────────

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    /// A private tree: `ws` is the child's working directory and workspace,
    /// `home` is a home outside it, and `ws/home` one inside it.
    fn new() -> Self {
        assert_eq!(
            aethershell::safety::current_mode(),
            aethershell::safety::Mode::Human,
            "these tests describe a parent that has set no mode; each child gets its own"
        );
        Self {
            root: tempfile::tempdir().expect("private fixture directory"),
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    fn workspace(&self) -> PathBuf {
        self.dir("ws")
    }

    /// A home outside the workspace: in agent mode nothing under it may be
    /// written.
    fn home_outside(&self) -> PathBuf {
        self.dir("home")
    }

    /// A home inside the workspace: the same store path, inside the jail.
    fn home_inside(&self) -> PathBuf {
        self.dir("ws").join("home")
    }

    /// The store path a child with `HOME = home` will use.
    fn store(&self, home: &Path) -> PathBuf {
        home.join(".aether").join("platform_db.json")
    }

    /// Put exact bytes at the store path, as an earlier write or a corruption
    /// would have.
    fn seed(&self, home: &Path, bytes: &[u8]) -> PathBuf {
        let path = self.store(home);
        std::fs::create_dir_all(path.parent().expect("the store has a parent"))
            .expect("store directory");
        std::fs::write(&path, bytes).expect("seed the store");
        path
    }
}

fn bytes_of(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The file's bytes, unchanged: the property every refusal here must have.
fn assert_untouched(path: &Path, before: &[u8], what: &str) {
    assert_eq!(bytes_of(path), before, "{what} changed the store");
}

/// The store document as JSON, read back from disk.
fn store_json(path: &Path) -> JsonValue {
    let text = String::from_utf8(bytes_of(path)).expect("the store is UTF-8");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("the store is not JSON: {e}\n{text}"))
}

// ── running one call in a child ─────────────────────────────────────────

#[derive(Clone, Copy)]
enum Profile {
    /// The gated profile, with the workspace jail on.
    Agent,
    /// The profile a person at a shell gets.
    Human,
}

/// Run one typed `call` in a child of this build and return its reply envelope.
fn call(cwd: &Path, home: &Path, profile: Profile, builtin: &str, args: JsonValue) -> JsonValue {
    let request = json!({ "action": "call", "builtin": builtin, "args": args }).to_string();

    let mut command = Command::new(env!("CARGO_BIN_EXE_ae"));
    command
        .arg("agent")
        .arg("execute")
        .current_dir(cwd)
        // The store this child reaches is the fixture's, never the operator's.
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env(
            "AETHER_MODE",
            match profile {
                Profile::Agent => "agent",
                Profile::Human => "human",
            },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Inherited operator settings that would decide this call's answer rather
    // than exercise it: an approval, RBAC, egress and the resource envelopes.
    // Removed from the child only.
    for name in [
        "AETHER_AGENT",
        "AETHER_APPROVE",
        "AETHER_APPROVE_ALL",
        "AETHER_AUDIT_LOG",
        "AETHER_MAX_OPS",
        "AETHER_MAX_FILES",
        "AETHER_MAX_PROCS",
        "AETHER_MAX_NET",
        "AETHER_TIMEOUT_MS",
        "AETHER_RBAC_CONFIG",
        "AETHER_PRINCIPAL",
        "AETHER_NET_ALLOW",
    ] {
        command.env_remove(name);
    }
    match profile {
        Profile::Agent => {
            command
                .env("AETHER_WORKSPACE", cwd)
                .env("AETHER_POLICY", "strict");
        }
        Profile::Human => {
            command
                .env_remove("AETHER_WORKSPACE")
                .env_remove("AETHER_POLICY");
        }
    }

    let mut child = OwnedChild(Some(
        command.spawn().expect("this build's own binary must start"),
    ));
    {
        let mut stdin = child.child_mut().stdin.take().expect("piped stdin");
        stdin
            .write_all(request.as_bytes())
            .expect("the request must reach the shell");
        // Dropping the handle is what tells the child the request ended.
    }
    let stdout = child.finish();
    let text = String::from_utf8(stdout).expect("the reply is UTF-8");
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("the reply must be JSON: {e}\nstdout: {text}"))
}

/// The same call in the profile each test names.
fn human(ws: &Path, home: &Path, builtin: &str, args: JsonValue) -> JsonValue {
    call(ws, home, Profile::Human, builtin, args)
}

fn agent(ws: &Path, home: &Path, builtin: &str, args: JsonValue) -> JsonValue {
    call(ws, home, Profile::Agent, builtin, args)
}

/// A child that is killed if the test leaves before the child is reaped.
struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("the child is still owned")
    }

    /// Drain both pipes and wait, killing the child if it overruns the bound.
    fn finish(&mut self) -> Vec<u8> {
        let mut child = self.0.take().expect("the child is still owned");
        let mut out = child.stdout.take().expect("piped stdout");
        let mut err = child.stderr.take().expect("piped stderr");
        let out_h = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = out.read_to_end(&mut bytes);
            bytes
        });
        let err_h = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = err.read_to_end(&mut bytes);
            bytes
        });

        let deadline = Instant::now() + BOUND;
        let status = loop {
            match child.try_wait().expect("wait on the child") {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("`ae agent execute` did not finish within {BOUND:?}");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let stdout = out_h.join().unwrap_or_default();
        let stderr = err_h.join().unwrap_or_default();
        assert!(
            status.success(),
            "`ae agent execute` exited with {:?}: {}",
            status.code(),
            String::from_utf8_lossy(&stderr)
        );
        stdout
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ── reading a reply ─────────────────────────────────────────────────────

/// The structured failure a reply carries.
fn failure(reply: &JsonValue) -> JsonValue {
    let text = reply["error"]
        .as_str()
        .unwrap_or_else(|| panic!("the reply carries no error: {reply}"));
    let parsed: JsonValue = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("the failure is not structured JSON: {e}\n{text}"));
    assert!(
        parsed["error"].is_object(),
        "the failure names no code: {text}"
    );
    parsed
}

/// The stable code of a refusal, read from both places the envelope carries it.
fn refusal_code(reply: &JsonValue) -> String {
    assert_eq!(
        reply["success"],
        json!(false),
        "a refusal must report success: false: {reply}"
    );
    let from_text = failure(reply)["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let from_metadata = reply["metadata"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        from_text, from_metadata,
        "the code disagrees between the error and the envelope: {reply}"
    );
    from_text
}

fn hint(reply: &JsonValue) -> String {
    failure(reply)["error"]["hint"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The result of a call that must have succeeded.
fn result(reply: &JsonValue) -> JsonValue {
    assert_eq!(reply["success"], json!(true), "the call failed: {reply}");
    reply["result"].clone()
}

// ── a store that cannot be read back ────────────────────────────────────

#[test]
fn a_store_that_is_not_json_is_reported_and_left_untouched() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());
    let store = fixture.seed(&home, b"{\"alpha\": 1,");
    let before = bytes_of(&store);

    // store, delete and import all rewrite the file; not one of them may do it
    // to a document it could not read.
    for (builtin, args) in [
        ("platform_db_store", json!(["beta", {"os": "linux"}])),
        ("platform_db_delete", json!(["alpha"])),
        ("platform_db_import", json!(["{\"gamma\": 2}"])),
    ] {
        let reply = human(&ws, &home, builtin, args);
        assert_eq!(refusal_code(&reply), "E_BAD_STATE", "{builtin}: {reply}");
        assert_untouched(&store, &before, builtin);
    }

    // The hint names the repair that exists. The generic `bad_state` hint names
    // a call to make first, which is not a repair for a damaged file.
    let reply = human(&ws, &home, "platform_db_load", json!(["alpha"]));
    let hint = hint(&reply);
    assert!(
        hint.contains("repair the store from backup"),
        "hint: {hint}"
    );
    assert!(
        !hint.contains("call `"),
        "the hint names a prerequisite call: {hint}"
    );
    assert_untouched(&store, &before, "load");
}

#[test]
fn a_store_that_is_json_but_not_an_object_is_not_read_as_empty() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());

    for content in [&b"[]"[..], &b"\"TOPSECRET\""[..], &b"17"[..]] {
        let store = fixture.seed(&home, content);
        let before = bytes_of(&store);
        let reply = human(&ws, &home, "platform_db_load", json!(["alpha"]));
        assert_eq!(refusal_code(&reply), "E_BAD_STATE", "load: {reply}");
        assert_untouched(&store, &before, "load");
    }

    // list answered `[]` for a store it could not parse, which is the same
    // answer an empty store gives.
    let store = fixture.seed(&home, b"[]");
    let before = bytes_of(&store);
    let listed = human(&ws, &home, "platform_db_list", json!([]));
    assert_eq!(refusal_code(&listed), "E_BAD_STATE", "{listed}");
    assert_untouched(&store, &before, "list");

    // The refusal names the path and the parser's diagnostic, never the
    // document's bytes: a store holds whatever a caller put in it.
    fixture.seed(&home, b"\"TOPSECRET\"");
    let echoed = human(&ws, &home, "platform_db_load", json!(["alpha"]));
    let text = echoed["error"].as_str().unwrap_or_default();
    assert!(
        !text.contains("TOPSECRET"),
        "the refusal published the store's contents: {text}"
    );
}

#[test]
fn bytes_that_are_not_utf8_are_an_io_error_and_the_file_survives() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());
    // `中文测试` in GBK: not valid UTF-8, so a reader that guessed is caught.
    let raw: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4];
    let store = fixture.seed(&home, raw);
    let before = bytes_of(&store);

    for (builtin, args) in [
        ("platform_db_store", json!(["beta", {"os": "linux"}])),
        ("platform_db_import", json!(["{\"gamma\": 2}"])),
        ("platform_db_init", json!([])),
    ] {
        let reply = human(&ws, &home, builtin, args);
        assert_eq!(refusal_code(&reply), "E_IO", "{builtin}: {reply}");
        assert_untouched(&store, &before, builtin);
    }
}

#[test]
fn init_validates_an_existing_store_and_never_rewrites_it() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());

    // A valid store is left byte for byte alone, and init reports its path.
    let store = fixture.seed(&home, b"{\n  \"alpha\": 1\n}");
    let before = bytes_of(&store);
    let reply = human(&ws, &home, "platform_db_init", json!([]));
    assert_eq!(
        Path::new(result(&reply).as_str().expect("a path")),
        store.as_path()
    );
    assert_untouched(&store, &before, "init");

    // A store that is not JSON is reported rather than replaced.
    fixture.seed(&home, b"{ not json");
    let corrupt = bytes_of(&store);
    let reply = human(&ws, &home, "platform_db_init", json!([]));
    assert_eq!(refusal_code(&reply), "E_BAD_STATE", "{reply}");
    assert_untouched(&store, &corrupt, "init");

    // With no store at all, init creates one.
    let fresh_home = fixture.dir("home_init");
    let fresh_store = fixture.store(&fresh_home);
    assert!(!fresh_store.exists());
    let reply = human(&ws, &fresh_home, "platform_db_init", json!([]));
    assert_eq!(bytes_of(&fresh_store), &b"{}"[..]);
    assert_eq!(
        Path::new(result(&reply).as_str().expect("a path")),
        fresh_store.as_path()
    );
}

// ── the answers a missing store has always given ────────────────────────

#[test]
fn a_missing_store_answers_null_and_an_empty_list_without_creating_anything() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());

    let loaded = human(&ws, &home, "platform_db_load", json!(["alpha"]));
    assert_eq!(result(&loaded), JsonValue::Null);
    let listed = human(&ws, &home, "platform_db_list", json!([]));
    assert_eq!(result(&listed), json!([]));
    assert!(
        !home.join(".aether").exists(),
        "a read created the store directory"
    );

    // A write is what creates it.
    let stored = human(
        &ws,
        &home,
        "platform_db_store",
        json!(["alpha", {"os": "linux"}]),
    );
    assert_eq!(result(&stored), json!(true));
    assert_eq!(
        store_json(&fixture.store(&home)),
        json!({"alpha": {"os": "linux"}})
    );
}

#[test]
fn a_store_round_trips_through_store_load_list_delete_and_import() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());
    let alpha = json!({"os": "linux", "cores": 4, "nested": {"arch": "x86_64"}});

    let stored = human(
        &ws,
        &home,
        "platform_db_store",
        json!(["alpha", alpha.clone()]),
    );
    assert_eq!(result(&stored), json!(true));
    let loaded = human(&ws, &home, "platform_db_load", json!(["alpha"]));
    assert_eq!(result(&loaded), alpha);
    let listed = human(&ws, &home, "platform_db_list", json!([]));
    assert_eq!(result(&listed), json!(["alpha"]));

    let stored = human(
        &ws,
        &home,
        "platform_db_store",
        json!(["beta", {"os": "macos"}]),
    );
    assert_eq!(result(&stored), json!(true));
    let listed = human(&ws, &home, "platform_db_list", json!([]));
    assert_eq!(result(&listed), json!(["alpha", "beta"]));

    // import merges into the store and answers its size.
    let imported = human(&ws, &home, "platform_db_import", json!(["{\"gamma\": 3}"]));
    assert_eq!(result(&imported), json!(3));
    let listed = human(&ws, &home, "platform_db_list", json!([]));
    assert_eq!(result(&listed), json!(["alpha", "beta", "gamma"]));

    // delete removes one key and reports whether it was there.
    let deleted = human(&ws, &home, "platform_db_delete", json!(["beta"]));
    assert_eq!(result(&deleted), json!(true));
    let loaded = human(&ws, &home, "platform_db_load", json!(["beta"]));
    assert_eq!(result(&loaded), JsonValue::Null);
    assert_eq!(
        store_json(&fixture.store(&home)),
        json!({"alpha": alpha, "gamma": 3})
    );
}

#[test]
fn deleting_an_absent_key_leaves_the_store_exactly_as_it_was() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());

    let store = fixture.seed(&home, b"{\n  \"alpha\": 1\n}");
    let before = bytes_of(&store);
    let reply = human(&ws, &home, "platform_db_delete", json!(["nope"]));
    assert_eq!(result(&reply), json!(false));
    assert_untouched(&store, &before, "a delete of an absent key");

    // With no store at all, the path is not created either.
    let absent_home = fixture.dir("home_absent");
    let reply = human(&ws, &absent_home, "platform_db_delete", json!(["alpha"]));
    assert_eq!(result(&reply), json!(false));
    assert!(
        !fixture.store(&absent_home).exists(),
        "a delete created an empty store"
    );
}

// ── the workspace jail, in a child of this build ────────────────────────

#[test]
fn an_agent_may_not_write_a_store_outside_its_workspace() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_outside());

    for (builtin, args) in [
        ("platform_db_init", json!([])),
        ("platform_db_store", json!(["alpha", {"os": "linux"}])),
        ("platform_db_import", json!(["{\"beta\": 2}"])),
    ] {
        let reply = agent(&ws, &home, builtin, args);
        let code = refusal_code(&reply);
        assert!(
            code == "E_OUTSIDE_WORKSPACE" || code == "E_POLICY_DENY",
            "{builtin} was not contained: {reply}"
        );
        assert!(
            !home.join(".aether").exists(),
            "{builtin} created the store directory before the guard"
        );
    }
}

#[test]
fn an_agent_write_inside_the_workspace_reaches_the_store() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_inside());
    let store = fixture.store(&home);

    let stored = agent(
        &ws,
        &home,
        "platform_db_store",
        json!(["alpha", {"os": "linux"}]),
    );
    assert_eq!(result(&stored), json!(true));
    // The bytes are read with std::fs; the builtin's own answer is not evidence
    // that it wrote anything.
    assert_eq!(store_json(&store), json!({"alpha": {"os": "linux"}}));

    let loaded = agent(&ws, &home, "platform_db_load", json!(["alpha"]));
    assert_eq!(result(&loaded), json!({"os": "linux"}));

    // init on an existing valid store reports the path and changes nothing.
    let before = bytes_of(&store);
    let reply = agent(&ws, &home, "platform_db_init", json!([]));
    assert_untouched(&store, &before, "init");
    assert_eq!(
        Path::new(result(&reply).as_str().expect("a path")),
        store.as_path()
    );

    let imported = agent(&ws, &home, "platform_db_import", json!(["{\"beta\": 2}"]));
    assert_eq!(result(&imported), json!(2));
    assert_eq!(
        store_json(&store),
        json!({"alpha": {"os": "linux"}, "beta": 2})
    );
}

#[test]
fn an_agent_delete_is_refused_and_a_human_delete_is_not() {
    let fixture = Fixture::new();
    let (ws, home) = (fixture.workspace(), fixture.home_inside());
    let store = fixture.seed(&home, b"{\n  \"alpha\": 1\n}");
    let before = bytes_of(&store);

    // In agent mode deleting from the store is a Destructive call with no
    // approval to present, so it is refused before the file is read.
    let refused = agent(&ws, &home, "platform_db_delete", json!(["alpha"]));
    assert_eq!(refusal_code(&refused), "E_NEEDS_APPROVAL", "{refused}");
    assert_untouched(&store, &before, "a refused delete");

    // The same call for a person at a shell runs, and writes the store back
    // without the key.
    let allowed = human(&ws, &home, "platform_db_delete", json!(["alpha"]));
    assert_eq!(result(&allowed), json!(true));
    assert_eq!(store_json(&store), json!({}));
}
