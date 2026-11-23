mod common;
use common::*;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn checked(f: &Fixture, args: &[&str]) -> std::process::Output {
    let o = f.run(args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o
}
fn record(f: &Fixture, event: &str, status: Option<&str>) {
    let mut data = json!({"pane_id":"p1"});
    if let Some(status) = status {
        data["agent_status"] = json!(status);
    }
    let mut c = f.command();
    c.arg("record").env(
        "HERDR_PLUGIN_EVENT_JSON",
        json!({"event":event,"data":data}).to_string(),
    );
    let o = output(c, Duration::from_secs(8));
    assert!(o.status.success(), "{o:?}");
}
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child(f: &Fixture, args: &[&str]) -> Guard {
    Guard(
        f.command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
#[test]
fn hooks_cache_legacy_permissions_and_server_isolation() {
    let f = Fixture::new("hooks");
    let path = f.cache_path();
    let legacy = f.path("agent-completions").join(path.file_name().unwrap());
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(
        &legacy,
        r#"{"p1":{"status":"idle","completed_at":42,"observed_at":42}}"#,
    )
    .unwrap();
    record(&f, "pane.focused", None);
    let entry = &f.times()["p1"];
    assert_eq!(entry["pending_done"], false);
    assert_eq!(entry["completed_at"], 42);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    record(&f, "pane.agent_status_changed", Some("working"));
    assert!(f.times()["p1"]["completed_at"].is_null());
    record(&f, "pane_agent_status_changed", Some("idle"));
    let entry = &f.times()["p1"];
    assert_eq!(entry["pending_done"], true);
    assert!((entry["completed_at"].as_f64().unwrap() - now()).abs() < 5.);
    record(&f, "pane.focused", None);
    assert_eq!(f.times()["p1"]["pending_done"], false);
    for event in ["pane.closed", "pane.exited", "pane.agent_detected"] {
        record(&f, "pane.agent_status_changed", Some("done"));
        record(&f, event, None);
        assert_eq!(f.times(), json!({}));
    }
    record(&f, "pane.agent_status_changed", Some("done"));
    let scope = f.context()["scope"].clone();
    fs::remove_file(f.path("session.sock")).unwrap();
    fs::write(f.path("session.sock"), "").unwrap();
    assert_ne!(f.cache_path(), path);
    assert_ne!(f.context()["scope"], scope);
    record(&f, "pane.agent_status_changed", Some("working"));
    let old: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(old["p1"]["status"], "done");
    assert_eq!(f.times()["p1"]["status"], "working");
}
#[test]
fn cli_and_invalid_preview_dimensions() {
    let mut f = Fixture::new("cli");
    f.env("FAKE_MODE", "runtime");
    let mut c = f.configure(Command::new("sh"));
    c.arg(Path::new(ROOT).join("run")).arg("open");
    assert!(output(c, Duration::from_secs(5)).status.success());
    assert_eq!(
        f.calls()[0],
        [
            "plugin",
            "pane",
            "open",
            "--plugin",
            "agent-picker",
            "--entrypoint",
            "picker"
        ]
    );
    checked(&f, &["check"]);
    assert_eq!(f.run(&["unknown"]).status.code(), Some(1));
    let mut c = f.configure(Command::new("sh"));
    c.arg(Path::new(ROOT).join("run")).arg("unknown");
    assert_eq!(output(c, Duration::from_secs(5)).status.code(), Some(2));
    let calls = f.calls();
    for cols in ["0", "-1", "65536", "invalid"] {
        let mut c = f.command();
        c.args([
            "--live-preview",
            f.path("preview.lock").to_str().unwrap(),
            "p1",
        ])
        .env("FZF_PREVIEW_COLUMNS", cols)
        .env("FZF_PREVIEW_LINES", "10");
        let o = output(c, Duration::from_secs(5));
        assert!(o.status.success());
        assert!(String::from_utf8_lossy(&o.stdout).contains("Preview unavailable"));
    }
    checked(
        &f,
        &[
            "--live-preview",
            f.path("preview.lock").to_str().unwrap(),
            "@section",
        ],
    );
    assert_eq!(f.calls(), calls);
}
#[test]
fn killed_preview_releases_attachment_and_lock() {
    let mut f = Fixture::new("kill-preview");
    f.env("FAKE_MODE", "runtime")
        .env("FZF_PREVIEW_COLUMNS", "43")
        .env("FZF_PREVIEW_LINES", "17");
    let lock = f.path("preview.lock");
    let mut p = child(
        &f,
        &[
            "--live-preview",
            lock.to_str().unwrap(),
            "p1",
            "other-origin",
        ],
    );
    until(|| f.path("attached").exists(), Duration::from_secs(5));
    p.0.kill().unwrap();
    p.0.wait().unwrap();
    until(|| f.path("released").exists(), Duration::from_secs(5));
    let pid = fs::read_to_string(f.path("attached"))
        .unwrap()
        .parse()
        .unwrap();
    until(|| !alive(pid), Duration::from_secs(4));
    assert!(lock_available(&lock));
    assert!(
        !f.calls().iter().any(|a| a.iter().any(|s| s == "--takeover")
            || [["agent", "focus"], ["pane", "write"], ["pane", "resize"]]
                .iter()
                .any(|p| a.starts_with(&p.map(str::to_owned))))
    );
}
#[test]
fn failed_attachment_spawn_releases_guard_promptly() {
    let mut f = Fixture::new("spawn-failure");
    f.env("FAKE_MODE", "runtime")
        .env("FZF_PREVIEW_COLUMNS", "43")
        .env("FZF_PREVIEW_LINES", "17")
        .env("FAIL_ATTACH_SPAWN", "1");
    let start = Instant::now();
    let lock = f.path("preview.lock");
    let o = checked(
        &f,
        &[
            "--live-preview",
            lock.to_str().unwrap(),
            "p1",
            "other-origin",
        ],
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("Preview unavailable"));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(lock_available(&lock));
}
#[test]
fn sqlite_worker_recovers_singleton_and_shuts_down_without_notifications() {
    let mut f = Fixture::new("worker");
    f.env("FAKE_MODE", "runtime");
    f.db(SCHEMA);
    let context = f.context();
    f.db(&format!("INSERT INTO reminders(id,scope,pane_id,title,due,context,status) VALUES ('fixture',{},'p1','Future synthetic alarm',{},{},'delivering')",sql(context["scope"].as_str().unwrap()),now()+3600.,sql(&context.to_string())));
    let state = f.path("state");
    let args = ["worker", state.to_str().unwrap()];
    let mut worker = child(&f, &args);
    until(
        || f.db_values("SELECT status FROM reminders")[0]["status"] == "pending",
        Duration::from_secs(5),
    );
    checked(&f, &args);
    assert!(worker.0.try_wait().unwrap().is_none());
    f.db("DELETE FROM reminders");
    until(
        || worker.0.try_wait().unwrap().is_some(),
        Duration::from_secs(4),
    );
    assert!(worker.0.wait().unwrap().success());
    assert_eq!(
        fs::metadata(state.join("reminders.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!f.call_text().contains("notification"));
}
#[test]
fn native_target_identity() {
    let data = fs::read(BINARY).unwrap();
    assert!(matches!(
        &data[..4],
        b"\x7fELF" | b"\xcf\xfa\xed\xfe" | b"\xfe\xed\xfa\xcf" | b"\xca\xfe\xba\xbe"
    ));
    let o = Command::new(BINARY).arg("--version").output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("herdr-agent-picker "));
}
