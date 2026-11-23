mod common;

use common::{local_format, local_time, lock_available, now, timestamp, Fixture, Pty, SCHEMA};
use regex::bytes::Regex;
use serde_json::json;
use std::thread;
use std::time::{Duration, Instant};

const ESC: &[u8] = b"\x1b";
const HEADER: &[u8] = b"Ctrl+D/U";
const HIGHLIGHT: &str = "\x1b[0;39;48;5;236m";

struct ReminderStore<'a>(&'a Fixture);

impl ReminderStore<'_> {
    fn pending(&self) -> Vec<serde_json::Value> {
        self.0.pending()
    }

    fn clean_up(&self) -> bool {
        self.0
            .db("DELETE FROM reminders WHERE status IN ('pending','delivering')");
        let lock = self.0.path("state/reminders-worker.lock");
        let until = Instant::now() + Duration::from_secs(4);
        while lock.exists() {
            if lock_available(&lock) {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
        true
    }
}

impl Drop for ReminderStore<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.clean_up()));
        } else {
            assert!(self.clean_up(), "detached reminder worker did not stop");
        }
    }
}

fn future_local_time() -> f64 {
    let now = now();
    let mut local = local_time(now);
    local.tm_mday += 2;
    local.tm_hour = 9;
    local.tm_min = 25;
    local.tm_sec = 0;
    let due = timestamp(local);
    assert!(due > now);
    due
}

fn return_to_picker(pty: &mut Pty, keys: &[u8]) {
    // The fresh header means fzf regained input ownership after the dialog.
    pty.clear();
    pty.send(keys);
    pty.wait_for(HEADER);
}

fn reminder_picker(scenario: &str) {
    let mut fixture = Fixture::new(scenario);
    fixture.env("FAKE_MODE", "reminder");
    fixture.write("config.toml", "");
    fixture.write_json("data.json", &json!({
        "agents": [{"agent": "demo", "pane_id": "demo:p1", "workspace_id": "demo",
            "tab_id": "demo:t1", "agent_status": "working", "title": "Fictional demo task", "focused": true}],
        "workspaces": [{"workspace_id": "demo", "label": "Demo"}],
        "tabs": [{"tab_id": "demo:t1", "label": "Example"}]
    }));
    fixture.db(SCHEMA);
    let store = ReminderStore(&fixture);
    let mut pty = fixture.spawn(&[], 26, 130);
    pty.wait_for(b"Fictional demo task");
    pty.clear();

    if scenario == "empty_manager" {
        pty.send(b"r");
        pty.wait_for(b"No scheduled alarms.");
        return_to_picker(&mut pty, ESC);
        pty.clear();
    }
    pty.send(b"a");
    pty.wait_for(b"Custom time");
    let dialog = pty.text();
    assert!(dialog.contains("Tomorrow 09:00") && dialog.contains(HIGHLIGHT));
    assert!(
        !dialog.contains("\x1b[7m"),
        "white reverse-video highlight returned"
    );
    assert!(
        !dialog.contains("\x1b[2J") && !dialog.contains("\x1b[?1049"),
        "dialog cleared the background"
    );
    let cursor = Regex::new(r"\x1b\[(\d+);(\d+)H").unwrap();
    let positions: Vec<_> = cursor
        .captures_iter(&pty.output)
        .map(|c| {
            let number = |group| {
                std::str::from_utf8(c.get(group).unwrap().as_bytes())
                    .unwrap()
                    .parse::<u16>()
                    .unwrap()
            };
            (number(1), number(2))
        })
        .collect();
    assert!(
        !positions.is_empty()
            && positions
                .iter()
                .all(|&(row, column)| (4..24).contains(&row) && column == 33),
        "{positions:?}"
    );

    if scenario == "delayed_jk" {
        pty.send(b"i");
        pty.drain(Duration::from_millis(100));
        assert!(!pty.text().contains("Input stopped:"));
    }
    if scenario == "resize" {
        pty.clear();
        pty.resize(20, 100);
        pty.wait_for(b"\x1b[4;18H");
        pty.send(b"\x1b[B\x1b[A");
    }
    match scenario {
        "escape" => {
            return_to_picker(&mut pty, ESC);
            assert!(store.pending().is_empty());
            pty.send(b"\r");
        }
        "custom" | "custom_cancel" | "custom_escape" | "custom_retry" | "delayed_custom" => {
            edit_custom_time(scenario, &mut pty, &store);
        }
        _ => preset(scenario, &mut pty, &store),
    }
    assert!(pty.finish().success(), "{}", pty.text());
    let expected_focus = if matches!(scenario, "custom_cancel" | "custom_escape") {
        None
    } else {
        Some("demo:p1")
    };
    assert_eq!(fixture.focus().as_deref(), expected_focus);
    assert!(!fixture.call_text().contains("notification"));
}

fn edit_custom_time(scenario: &str, pty: &mut Pty, store: &ReminderStore<'_>) {
    pty.clear();
    pty.send(b"8");
    pty.wait_for(b"Date and time (local)");
    pty.wait_for(b"Date:");
    pty.wait_for(b"Time:");
    let due = future_local_time();
    if scenario == "custom_retry" {
        pty.clear();
        pty.send(b"\x1b[H2000\r");
        pty.wait_for(b"Choose a future date and time");
    }
    if matches!(scenario, "custom_cancel" | "custom_escape") {
        if scenario == "custom_escape" {
            pty.send(b"12");
        }
        return_to_picker(pty, ESC);
        assert!(store.pending().is_empty());
        pty.send(ESC);
        return;
    }
    if scenario == "delayed_custom" {
        pty.drain(Duration::from_millis(1400));
    }
    let year = local_format(due, "%Y");
    pty.clear();
    pty.send(format!("\x1b[H{year}").as_bytes());
    pty.wait_for(format!("[{year}]").as_bytes());
    let fields = local_format(due, "\t%m\t%d\t%H\t%M");
    pty.send(fields.as_bytes());
    pty.wait_for(b"[25]");
    schedule(pty, b"\r", store);
    assert_eq!(store.pending()[0]["due"].as_f64().unwrap(), due);
    show_badge(pty);
    pty.send(b"\r");
}

fn preset(scenario: &str, pty: &mut Pty, store: &ReminderStore<'_>) {
    let started = now();
    let moved = matches!(scenario, "jk" | "delayed_jk" | "delayed_arrows");
    if moved {
        if scenario.starts_with("delayed") {
            pty.drain(Duration::from_millis(1400));
        }
        let keys: [&[u8]; 3] = if scenario == "delayed_arrows" {
            [b"\x1b[B", b"\x1b[B", b"\x1b[A"]
        } else {
            [b"j", b"j", b"k"]
        };
        for (key, label) in
            keys.into_iter()
                .zip(["[2] 10 minutes", "[3] 15 minutes", "[2] 10 minutes"])
        {
            pty.clear();
            pty.send(key);
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                pty.drain(Duration::from_millis(50));
                if pty
                    .text()
                    .split(HIGHLIGHT)
                    .skip(1)
                    .any(|chunk| chunk.split("\x1b[0m").next().unwrap().contains(label))
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "expected highlighted {label}: {}",
                    pty.text()
                );
            }
        }
    }
    schedule(pty, if moved { b"\r" } else { b"1" }, store);
    let duration = if moved { 600.0 } else { 300.0 };
    let due = store.pending()[0]["due"].as_f64().unwrap();
    assert!(
        started + duration - 5.0 < due && due < now() + duration + 5.0,
        "unexpected due timestamp {due}"
    );
    show_badge(pty);
    assert!(
        store.0.focus().is_none(),
        "setting an alarm focused the agent"
    );
    pty.clear();
    if scenario == "manager" {
        pty.send(b"r");
        pty.wait_for(b"Scheduled alarms");
        return_to_picker(pty, b"\r");
    } else {
        pty.send(b"a");
        pty.wait_for(b"Cancel current alarm");
        return_to_picker(pty, b"9");
    }
    assert!(store.pending().is_empty(), "cancellation did not persist");
    pty.send(b"\r");
}

fn schedule(pty: &mut Pty, keys: &[u8], store: &ReminderStore<'_>) {
    pty.clear();
    pty.send(keys);
    pty.wait_until(|| !store.pending().is_empty(), Duration::from_secs(8));
    pty.wait_for(HEADER);
}

fn show_badge(pty: &mut Pty) {
    // Reveal the end-of-row badge in the narrow fixture.
    pty.clear();
    pty.send(b"p");
    pty.wait_for(b"[alarm ");
}

macro_rules! scenarios {
    ($($case:ident),+ $(,)?) => { $(
        #[test]
        fn $case() {
            reminder_picker(stringify!($case));
        }
    )+ };
}

scenarios!(
    preset_cancel,
    custom,
    custom_cancel,
    manager,
    escape,
    custom_escape,
    custom_retry,
    resize,
    empty_manager,
    jk,
    delayed_jk,
    delayed_arrows,
    delayed_custom,
);
