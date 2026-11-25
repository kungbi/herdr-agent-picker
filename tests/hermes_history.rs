mod common;
use common::*;
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
};

#[test]
fn pages_public_bound_session_and_rejects_new_session() {
    let mut fixture = Fixture::new("semantic-history");
    fixture.env("FAKE_MODE", "interactive");
    fixture.write_json(
        "data.json",
        &json!({"pid": 777, "agents": [{"pane_id":"p1", "terminal_id":"t1"}]}),
    );
    let visible: Vec<String> = (84..=120).map(|n| format!("line {n:03}")).collect();
    let mut visible = visible;
    visible[0] = "\x1b[35mline 084\x1b[0m".into();
    for row in &mut visible {
        row.insert_str(0, "    ");
    }
    visible.push("NATIVE STATUS".into());
    fixture.write("content", &visible.join("\n"));
    fixture.env("CONTENT", fixture.path("content"));
    let home = fixture.path("home/.hermes/profiles/work");
    let bindings = home.join("cache/picker-bindings");
    fs::create_dir_all(&bindings).unwrap();
    let marker = fixture.path("hermes-tui-active-session-fixture");
    let mut active = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&marker)
        .unwrap();
    write!(active, "{}", json!({"session_id":"session-a"})).unwrap();
    drop(active);
    let mut lease = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(bindings.join("777"))
        .unwrap();
    write!(lease, "{}", json!({"pane":"p1", "marker":marker})).unwrap();
    assert_eq!(unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX) }, 0);
    let answer = (1..=120)
        .map(|n| format!("line {n:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    sqlite(&home.join("state.db"), &format!("CREATE TABLE messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,content TEXT,active INTEGER,_compressed_summary INTEGER,display_kind TEXT,tool_calls TEXT); INSERT INTO messages VALUES(1,'session-a','user','actual question',1,0,NULL,NULL); INSERT INTO messages VALUES(2,'session-a','assistant',{},1,0,NULL,NULL); INSERT INTO messages VALUES(3,'session-b','user','wrong conversation',1,0,NULL,NULL);", sql(&answer)));
    let base = fixture.path("viewport");
    fs::write(base.with_extension("extent"), "37 37").unwrap();
    fs::write(
        base.with_extension("window"),
        json!({"pane":"p1","terminal":null,"width":80,"limit":74,"exhausted":false,"rows":visible})
            .to_string(),
    )
    .unwrap();
    let base = base.to_str().unwrap();
    let result = fixture.run(&["--scroll", base, "-2", "p1"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let window: Value =
        serde_json::from_slice(&fs::read(fixture.path("viewport.window")).unwrap()).unwrap();
    let merged: Vec<String> = serde_json::from_value(window["rows"].clone()).unwrap();
    assert!(
        merged.ends_with(&visible),
        "native styling and status must survive paging"
    );
    assert_eq!(window["semantic"][1], "session-a");
    assert!(!window.to_string().contains("wrong conversation"));
    let mut seen_question = false;
    for _ in 0..10 {
        let frame: Value =
            serde_json::from_slice(&fs::read(fixture.path("viewport.history.frame")).unwrap())
                .unwrap();
        let body = frame[3].as_str().unwrap();
        assert!(body.lines().count() <= 37);
        seen_question |= body.contains("actual question");
        if seen_question {
            break;
        }
        assert!(fixture
            .run(&["--scroll", base, "-3", "p1"])
            .status
            .success());
    }
    assert!(
        seen_question,
        "question must be reachable above the 120-line answer"
    );
    fs::write(&marker, json!({"session_id":"session-b"}).to_string()).unwrap();
    let before = fs::read(fixture.path("viewport.history.frame")).unwrap();
    assert!(fixture
        .run(&["--scroll", base, "-3", "p1"])
        .status
        .success());
    assert_eq!(
        fs::read(fixture.path("viewport.history.frame")).unwrap(),
        before
    );
}
