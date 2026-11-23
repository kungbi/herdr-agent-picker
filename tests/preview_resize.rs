mod common;

use common::{Fixture, Pty};
use regex::Regex;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

const WAIT: Duration = Duration::from_secs(8);
const STEP: Duration = Duration::from_millis(30);

struct Screen {
    cells: Vec<Vec<char>>,
    frame: Vec<Vec<char>>,
    cursor: (usize, usize),
    saved: (usize, usize),
    pending: Vec<u8>,
    csi: Regex,
    row_pattern: Regex,
}

impl Screen {
    fn new(rows: usize, columns: usize) -> Self {
        let cells = vec![vec![' '; columns]; rows];
        Self {
            frame: cells.clone(),
            cells,
            cursor: (0, 0),
            saved: (0, 0),
            pending: Vec::new(),
            csi: Regex::new(r"^\x1b\[([0-?]*)([ -/]*)([@-~])").unwrap(),
            row_pattern: Regex::new(r"ROW_[0-9]{2}(?:_EDIT)?").unwrap(),
        }
    }

    fn feed(&mut self, data: &[u8]) {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(data);
        let mut bytes = bytes.as_slice();
        while !bytes.is_empty() {
            let consumed = if bytes.starts_with(b"\x1b[") {
                let Some(end) = bytes[2..]
                    .iter()
                    .position(|byte| (b'@'..=b'~').contains(byte))
                else {
                    break;
                };
                let length = end + 3;
                let sequence = std::str::from_utf8(&bytes[..length]).unwrap();
                let captures = self
                    .csi
                    .captures(sequence)
                    .unwrap_or_else(|| panic!("bad CSI {sequence:?}"));
                self.control(&captures[1], captures[3].as_bytes()[0]);
                length
            } else if bytes[0] == 27 {
                let Some(escape) = bytes.get(1) else { break };
                match escape {
                    b']' => {
                        let Some(end) = (2..bytes.len())
                            .find(|&i| bytes[i] == 7 || bytes[i..].starts_with(b"\x1b\\"))
                        else {
                            break;
                        };
                        end + if bytes[end] == 7 { 1 } else { 2 }
                    }
                    b'7' => {
                        self.saved = self.cursor;
                        2
                    }
                    b'8' => {
                        self.cursor = self.saved;
                        2
                    }
                    b'(' | b')' if bytes.len() >= 3 => 3,
                    b'(' | b')' => break,
                    other => panic!("unsupported escape {other:?}"),
                }
            } else {
                let width = match bytes[0] {
                    0..=127 => 1,
                    194..=223 => 2,
                    224..=239 => 3,
                    240..=244 => 4,
                    _ => panic!("invalid UTF-8 in terminal output: {bytes:?}"),
                };
                if bytes.len() < width {
                    break;
                }
                let ch = std::str::from_utf8(&bytes[..width])
                    .unwrap()
                    .chars()
                    .next()
                    .unwrap();
                let (row, col) = &mut self.cursor;
                match ch {
                    '\r' => *col = 0,
                    '\n' => *row = (*row + 1).min(self.cells.len() - 1),
                    '\x08' => *col = col.saturating_sub(1),
                    _ => {
                        if let Some(width @ 1..) = ch.width() {
                            self.cells[*row][*col] = ch;
                            *col = (*col + width).min(self.cells[0].len() - 1);
                        }
                    }
                }
                width
            };
            bytes = &bytes[consumed..];
        }
        self.pending.extend_from_slice(bytes);
    }

    fn control(&mut self, args: &str, action: u8) {
        if args.starts_with('?') {
            if args == "?7" && action == b'h' {
                // fzf restores auto-wrap at the end of a repaint.
                self.frame.clone_from(&self.cells);
            }
            return;
        }
        let mut values = args
            .split(';')
            .map(|value| value.parse::<usize>().unwrap_or(0));
        let code = values.next().unwrap_or(0);
        let n = code.max(1);
        let (row, col) = &mut self.cursor;
        let columns = self.cells[0].len();
        match action {
            b'A' => *row = row.saturating_sub(n),
            b'B' => *row += n,
            b'C' => *col += n,
            b'D' => *col = col.saturating_sub(n),
            b'H' | b'f' => {
                *row = n - 1;
                *col = values.next().unwrap_or(0).max(1) - 1;
            }
            b'G' => *col = n - 1,
            b'd' => *row = n - 1,
            b'J' => match code {
                0 => {
                    self.cells[*row][*col..].fill(' ');
                    for line in &mut self.cells[*row + 1..] {
                        line.fill(' ');
                    }
                }
                2 | 3 => {
                    for line in &mut self.cells {
                        line.fill(' ');
                    }
                }
                _ => panic!("unsupported erase: {args}"),
            },
            b'K' | b'X' => {
                let (start, end) = match (action, code) {
                    (b'X', _) => (*col, (*col + n).min(columns)),
                    (b'K', 1) => (0, *col + 1),
                    (b'K', 2) => (0, columns),
                    _ => (*col, columns),
                };
                self.cells[*row][start..end].fill(' ');
            }
            b's' => self.saved = (*row, *col),
            b'u' => (*row, *col) = self.saved,
            b'm' | b'h' | b'l' | b'n' | b't' | b'q' => {}
            _ => panic!("unsupported CSI: {args} {}", char::from(action)),
        }
        *row = (*row).min(self.cells.len() - 1);
        *col = (*col).min(columns - 1);
    }

    fn preview(&self) -> Vec<String> {
        let text = self
            .frame
            .iter()
            .map(|row| row[43..97].iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        self.row_pattern
            .find_iter(&text)
            .map(|row| row.as_str().to_owned())
            .collect()
    }

    fn pump(&mut self, picker: &mut Pty) {
        picker.drain(STEP);
        self.feed(&picker.output);
        picker.clear();
    }
}

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

    picker.send(&b"\x1b[1;2A".repeat(5));
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        !has(rows, "ROW_30") && has(rows, "ROW_12")
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
