mod common;

use common::{Fixture, Pty, Screen};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(8);

fn setup(label: &str) -> Fixture {
    let mut fixture = Fixture::new(label);
    let data = fixture.path("snapshot.json");
    let events = fixture.path("events");
    fixture
        .env("FAKE_MODE", "resize")
        .env("DATA", data)
        .env("EVENTS", events);
    fixture
}

fn agent(pane: &str, title: &str, focused: bool) -> Value {
    json!({"agent": "claude", "pane_id": pane, "workspace_id": "w1", "tab_id": "t1",
           "agent_status": "working", "title": title, "focused": focused})
}

fn reads(fixture: &Fixture, pane: &str) -> usize {
    fixture
        .events()
        .iter()
        .filter(|line| *line == &format!("read {pane}"))
        .count()
}

fn attachments(fixture: &Fixture, pane: &str) -> Vec<Attachment> {
    fixture
        .events()
        .iter()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if let ["attach", target, cols, rows, fzf_cols, fzf_rows, pid] = fields.as_slice() {
                if pane.is_empty() || *target == pane {
                    return Some(Attachment {
                        size: [cols.parse().unwrap(), rows.parse().unwrap()],
                        advertised_size: [fzf_cols.parse().unwrap(), fzf_rows.parse().unwrap()],
                        pid: pid.parse().unwrap(),
                    });
                }
            }
            None
        })
        .collect()
}

fn assert_preview_arguments(fixture: &Fixture) {
    let calls = fixture.calls();
    for call in &calls {
        match call
            .get(..2)
            .map(|pair| (pair[0].as_str(), pair[1].as_str()))
        {
            Some(("terminal", "attach")) => assert_eq!(call.len(), 3, "{call:?}"),
            Some(("pane", "read")) => assert_eq!(
                call[3..],
                [
                    "--source",
                    "recent-unwrapped",
                    "--lines",
                    "30",
                    "--format",
                    "ansi"
                ],
                "{call:?}"
            ),
            _ => {}
        }
    }
}

#[derive(Debug)]
struct Attachment {
    size: [u16; 2],
    advertised_size: [u16; 2],
    pid: libc::pid_t,
}

impl Attachment {
    fn gone(&self) -> bool {
        // Signal zero checks process existence without delivering a signal.
        let result = unsafe { libc::kill(self.pid, 0) };
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    fn assert_size(&self) {
        assert_eq!(self.size, self.advertised_size, "{self:?}");
    }
}

fn publish_rows(fixture: &Fixture, first: u8, last: u8, suffix: &str) {
    let content: String = (first..=last)
        .map(|row| format!("\x1b[32mROW_{row:02}{suffix}\x1b[0m\n"))
        .collect();
    fixture.write("content", &content);
}

fn wait_screen(
    fixture: &Fixture,
    picker: &mut Pty,
    screen: &mut Screen,
    ready: impl Fn(&[String]) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        screen.pump(picker);
        if ready(&screen.preview()) {
            return;
        }
    }
    panic!(
        "preview {:?}, Herdr events {:?}",
        screen.preview(),
        fixture.events()
    );
}

fn assert_stable(
    fixture: &Fixture,
    picker: &mut Pty,
    screen: &mut Screen,
    expected: &[String],
    refreshes: usize,
) {
    let count = reads(fixture, "p1");
    let deadline = Instant::now() + WAIT;
    let end = Instant::now() + Duration::from_millis(refreshes as u64 * 1000 + 300);
    while reads(fixture, "p1") < count + refreshes || Instant::now() < end {
        assert!(Instant::now() < deadline, "{:?}", fixture.events());
        screen.pump(picker);
        assert_eq!(screen.preview(), expected, "{:?}", fixture.events());
    }
}

fn keeps_preview_viewport(origin: &str) {
    let mut fixture = setup(origin);
    let content = fixture.path("content");
    fixture.env("CONTENT", content);
    fixture.write_json(
        "snapshot.json",
        &json!({
            "focused_pane_id": origin,
            "panes": [{"pane_id": "p1"}, {"pane_id": "outside"}],
            "agents": [agent("p1", "Agent", true)],
            "workspaces": [], "tabs": []
        }),
    );
    publish_rows(&fixture, 1, 30, "");
    let mut picker = fixture.spawn(&["picker"], 18, 100);
    let mut screen = Screen::new(18, 100);
    let has = |rows: &[String], value: &str| rows.iter().any(|row| row == value);
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        has(rows, "ROW_30")
    });
    assert!(!has(&screen.preview(), "ROW_01"));

    let bottom = screen.preview();
    picker.send(&b"\x1b[1;2A".repeat(5));
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        !has(rows, "ROW_30") && rows.iter().any(|row| !bottom.contains(row))
    });
    let end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < end {
        screen.pump(&mut picker);
    }
    let scrolled = screen.preview();
    assert_stable(&fixture, &mut picker, &mut screen, &scrolled, 3);
    publish_rows(&fixture, 1, 33, "");
    assert_stable(&fixture, &mut picker, &mut screen, &scrolled, 3);
    publish_rows(&fixture, 5, 34, "");
    assert_stable(&fixture, &mut picker, &mut screen, &scrolled, 2);

    picker.send(&b"\x1b[1;2B".repeat(50));
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        has(rows, "ROW_34")
    });
    publish_rows(&fixture, 6, 35, "");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        has(rows, "ROW_35")
    });
    publish_rows(&fixture, 1, 30, "_EDIT");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        has(rows, "ROW_30_EDIT")
    });
    assert!(screen.preview().iter().all(|row| row.ends_with("_EDIT")));
    let attached = attachments(&fixture, "");
    assert_eq!(
        attached.len(),
        usize::from(origin == "outside"),
        "{:?}",
        fixture.events()
    );

    picker.send(b"q");
    assert!(picker.finish().success());
    for process in attached {
        picker.wait_until(|| process.gone(), WAIT);
    }
    assert_preview_arguments(&fixture);
}

#[test]
fn origin_viewport() {
    keeps_preview_viewport("p1");
}

#[test]
fn attached_viewport() {
    keeps_preview_viewport("outside");
}

#[test]
fn tall_preview_fills_with_recent_history() {
    let mut fixture = setup("tall-preview");
    let content = fixture.path("content");
    fixture.env("CONTENT", content);
    fixture.write_json(
        "snapshot.json",
        &json!({
            "focused_pane_id": "p1", "panes": [{"pane_id": "p1"}],
            "agents": [agent("p1", "Agent", true)], "workspaces": [], "tabs": []
        }),
    );
    publish_rows(&fixture, 1, 90, "");
    let mut picker = fixture.spawn(&["picker"], 48, 100);
    let mut screen = Screen::new(48, 100);
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.len() > 30 && rows.last().is_some_and(|row| row == "ROW_90")
    });
    let rows = screen.preview();
    let requested = fixture
        .calls()
        .into_iter()
        .rev()
        .find(|c| c.starts_with(&["pane".into(), "read".into()]))
        .unwrap();
    assert_eq!(rows.len(), requested[6].parse::<usize>().unwrap());
    picker.send(b"q");
    assert!(picker.finish().success());
}

fn lifecycle(scenario: &str) {
    let mut fixture = setup(scenario);
    if scenario == "read-failure" {
        fixture.env("FAIL_READ", "1");
    }
    if scenario == "attach-failure" {
        fixture.env("FAIL_ATTACH", "1");
    }
    let preview = format!(
        "{} --live-preview {} {{1}} outside",
        common::quote(env!("CARGO_BIN_EXE_herdr-agent-picker")),
        common::quote(fixture.path("preview.lock").to_str().unwrap())
    );
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "printf 'p1\\np2\\n' | exec fzf --no-tty-default --layout=reverse --preview \"$1\" --preview-window=right:60% --bind=p:toggle-preview,q:abort,resize:refresh-preview", "test", &preview]);
    let command = fixture.configure(command);
    let mut picker = Pty::spawn(command, 28, 100);

    if scenario == "attach-failure" {
        picker.wait_for(b"Preview unavailable");
        assert!(!fixture
            .events()
            .iter()
            .any(|line| line.starts_with("read ")));
        picker.send(b"q");
        picker.finish();
        assert_preview_arguments(&fixture);
        return;
    }

    picker.wait_until(|| reads(&fixture, "p1") > 0, WAIT);
    let first = attachments(&fixture, "p1").remove(0);
    first.assert_size();
    let log = fixture.events();
    let read = log.iter().position(|line| line == "read p1").unwrap();
    let attach = log
        .iter()
        .position(|line| line.starts_with("attach p1 "))
        .unwrap();
    assert!(read > attach, "{log:?}");

    match scenario {
        "read-failure" => {
            picker.wait_until(|| first.gone(), WAIT);
            picker.send(b"q");
        }
        "interrupt" => {
            picker.send(b"\x03");
            picker.wait_until(|| first.gone(), WAIT);
        }
        "switch-hide-close" => {
            picker.send(b"\x1b[B");
            picker.wait_until(|| reads(&fixture, "p2") > 0, WAIT);
            picker.wait_until(|| first.gone(), WAIT);
            let log = fixture.events();
            let release = log.iter().position(|line| line == "release p1").unwrap();
            let attach = log
                .iter()
                .position(|line| line.starts_with("attach p2 "))
                .unwrap();
            assert!(release < attach, "{log:?}");
            let second = attachments(&fixture, "p2").remove(0);

            picker.resize(28, 72);
            picker.wait_until(|| attachments(&fixture, "p2").len() == 2, WAIT);
            let resized = attachments(&fixture, "p2").pop().unwrap();
            resized.assert_size();
            assert_ne!(resized.size[0], second.size[0], "{second:?} -> {resized:?}");
            picker.wait_until(|| second.gone(), WAIT);

            picker.send(b"p");
            picker.wait_until(|| resized.gone(), WAIT);
            let read_count = || {
                fixture
                    .events()
                    .iter()
                    .filter(|line| line.starts_with("read "))
                    .count()
            };
            let before = read_count();
            picker.drain(Duration::from_millis(1300));
            assert_eq!(read_count(), before);

            picker.send(b"p");
            picker.wait_until(|| attachments(&fixture, "p2").len() == 3, WAIT);
            let third = attachments(&fixture, "p2").pop().unwrap();
            picker.send(b"q");
            picker.wait_until(|| third.gone(), WAIT);
        }
        _ => unreachable!(),
    }
    picker.finish();
    assert_preview_arguments(&fixture);
}

#[test]
fn switches_hides_resizes_and_closes_attachment() {
    lifecycle("switch-hide-close");
}

#[test]
fn releases_attachment_on_interrupt() {
    lifecycle("interrupt");
}

#[test]
fn releases_attachment_on_read_failure() {
    lifecycle("read-failure");
}

#[test]
fn does_not_read_after_attach_failure() {
    lifecycle("attach-failure");
}

fn assert_no_get_or_attach(log: &[String]) {
    assert!(
        !log.iter()
            .any(|line| line.starts_with("get ") || line.starts_with("attach ")),
        "{log:?}"
    );
}

fn assert_native_origin(log: &[String]) {
    assert!(
        !log.iter()
            .any(|line| ["get p2", "attach p2", "attach popup"]
                .iter()
                .any(|prefix| line.starts_with(prefix))),
        "{log:?}"
    );
}

fn set_origin_snapshot(fixture: &Fixture, snapshot: &Value) {
    fixture.write_json("snapshot.json", snapshot);
}

fn picker_origin(scenario: &str) {
    let mut fixture = setup(scenario);
    fixture
        .env("HERDR_PLUGIN_ID", "agent-picker")
        .env("HERDR_PLUGIN_ENTRYPOINT_ID", "picker")
        .env("HERDR_PANE_ID", "popup");
    let session_focus = match scenario {
        "plugin-origin" | "popup-only" => Some("popup"),
        "context-only" | "missing-origin" => None,
        _ => Some("p2"),
    };
    let context_pane = match scenario {
        "plugin-origin" | "context-only" => Some("p2"),
        "conflicting-origin" => Some("p1"),
        "stale-context" => Some("stale"),
        _ => None,
    };
    if let Some(pane) = context_pane {
        fixture.env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            json!({"focused_pane_id": pane}).to_string(),
        );
    }
    if scenario == "origin-read-failure" {
        fixture.env("FAIL_READ", "1");
    }
    let mut snapshot = json!({
        "agents": [agent("p1", "Agent p1", true), agent("p2", "Agent p2", scenario != "origin-not-selected")],
        "panes": [{"pane_id": "p1"}, {"pane_id": "p2"}, {"pane_id": "popup"}],
        "workspaces": [{"workspace_id": "w1", "label": "Workspace"}],
        "tabs": [{"tab_id": "t1", "label": "Tab"}]
    });
    if let Some(pane) = session_focus {
        snapshot["focused_pane_id"] = json!(pane);
    }
    set_origin_snapshot(&fixture, &snapshot);

    let mut picker = fixture.spawn(&["picker"], 28, 100);
    let known_origin = matches!(
        scenario,
        "session-origin" | "origin-not-selected" | "plugin-origin" | "context-only"
    );
    let initial_attachments = usize::from(scenario == "origin-not-selected");
    let reads_p1 = || reads(&fixture, "p1");
    let reads_p2 = || reads(&fixture, "p2");
    if initial_attachments == 1 {
        picker.wait_until(|| reads_p1() >= 1, WAIT);
        let first = attachments(&fixture, "p1").remove(0);
        picker.send(b"j");
        picker.wait_until(|| reads_p2() >= 1, WAIT);
        picker.wait_until(|| first.gone(), WAIT);
    }
    picker.wait_until(|| reads_p2() >= 1, WAIT);
    let log = fixture.events();
    let native_start = log.len();
    assert_native_origin(&log);
    if initial_attachments == 0 {
        assert_no_get_or_attach(&log);
    }

    let mut final_attach = None;
    if scenario == "origin-read-failure" {
        picker.wait_for(b"Preview unavailable");
        assert_no_get_or_attach(&fixture.events()[native_start..]);
    } else {
        picker.wait_until(|| reads_p2() >= 2, WAIT);
        picker.wait_for(b"SCREEN_p2");
        assert_no_get_or_attach(&fixture.events()[native_start..]);
        let before = reads_p2();
        picker.resize(28, 82);
        picker.wait_until(|| reads_p2() > before, WAIT);
        assert_no_get_or_attach(&fixture.events()[native_start..]);

        picker.send(b"p");
        picker.drain(Duration::from_millis(300));
        let before = reads_p2();
        picker.drain(Duration::from_millis(1300));
        assert_eq!(reads_p2(), before, "{:?}", fixture.events());
        picker.send(b"p");
        picker.wait_until(|| reads_p2() > before, WAIT);

        let before = reads_p1();
        picker.send(b"k");
        picker.wait_until(|| reads_p1() > before, WAIT);
        if known_origin {
            let process = attachments(&fixture, "p1").pop().unwrap();
            process.assert_size();
            snapshot["focused_pane_id"] = json!("p1");
            snapshot["agents"][0]["title"] = json!("Changed agent p1");
            set_origin_snapshot(&fixture, &snapshot);
            picker.wait_until(
                || fixture.events().iter().any(|line| line == "snapshot p1"),
                WAIT,
            );
            // A backend read can finish before fzf applies the title reload.
            // Wait for the painted label before testing a new navigation key.
            picker.wait_for(b"Changed");
            let before = reads_p1();
            picker.wait_until(|| reads_p1() > before, WAIT);
        } else {
            assert_no_get_or_attach(&fixture.events());
        }

        let before = reads_p2();
        picker.send(b"j");
        picker.wait_until(|| reads_p2() > before, WAIT);
        if known_origin {
            for process in attachments(&fixture, "p1") {
                picker.wait_until(|| process.gone(), WAIT);
            }
            let log = fixture.events();
            let release = log.iter().rposition(|line| line == "release p1").unwrap();
            let last_read = log.iter().rposition(|line| line == "read p2").unwrap();
            assert!(release < last_read, "{log:?}");
        }
        assert_native_origin(&fixture.events());
        let before = reads_p2();
        picker.wait_until(|| reads_p2() > before, WAIT);
        if known_origin {
            // Reloading the changed title can restart p1's preview before this
            // switch. Wait for an attachment caused by this key press.
            let before = attachments(&fixture, "p1").len();
            assert!(before > initial_attachments);
            picker.send(b"k");
            picker.wait_until(|| attachments(&fixture, "p1").len() > before, WAIT);
            final_attach = attachments(&fixture, "p1").pop();
        }
    }

    picker.send(b"q");
    assert!(picker.finish().success());
    if let Some(process) = final_attach {
        common::until(|| process.gone(), WAIT);
    }
    let stopped = fixture.events();
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        fixture.events(),
        stopped,
        "Polling continued after picker close"
    );
    assert_preview_arguments(&fixture);
    assert!(
        !stopped.iter().any(|line| line.starts_with("unexpected ")),
        "{stopped:?}"
    );
}

#[test]
fn session_origin() {
    picker_origin("session-origin");
}

#[test]
fn origin_not_selected() {
    picker_origin("origin-not-selected");
}

#[test]
fn plugin_origin() {
    picker_origin("plugin-origin");
}

#[test]
fn context_only() {
    picker_origin("context-only");
}

#[test]
fn missing_origin() {
    picker_origin("missing-origin");
}

#[test]
fn conflicting_origin() {
    picker_origin("conflicting-origin");
}

#[test]
fn stale_context() {
    picker_origin("stale-context");
}

#[test]
fn popup_only() {
    picker_origin("popup-only");
}

#[test]
fn origin_read_failure() {
    picker_origin("origin-read-failure");
}
