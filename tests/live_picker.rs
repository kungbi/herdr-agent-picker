//! Exercise live reloads using real fzf and a mutable fake Herdr snapshot.
mod common;

use common::{now, Fixture};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn snapshot(agents: &[(u32, &str, &str)]) -> Value {
    let agents: Vec<_> = agents
        .iter()
        .map(|(number, status, title)| {
            json!({
                "agent": "claude",
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "pane_id": format!("w1:p{number}"),
                "agent_status": status,
                "title": title,
                "focused": *number == 1,
            })
        })
        .collect();
    json!({
        "agents": agents,
        "workspaces": [{"workspace_id": "w1", "label": "Workspace"}],
        "tabs": [{"tab_id": "w1:t1", "label": "Tab"}],
    })
}

fn updated_snapshot(scenario: &str) -> Value {
    if scenario == "becomes_empty" {
        return snapshot(&[]);
    }
    if scenario == "third_done_via_idle" {
        return snapshot(&[
            (1, "done", "Finished task"),
            (2, "done", "Second finish"),
            (3, "idle", "Third completion"),
        ]);
    }
    let mut updated = snapshot(&[
        (1, "done", "Finished task"),
        (2, "working", "Continuing task"),
        (3, "idle", "Updated third task"),
        (4, "blocked", "New blocked task"),
    ]);
    let agents = updated["agents"].as_array_mut().unwrap();
    if scenario == "done_only" {
        agents.retain(|agent| matches!(agent["agent_status"].as_str(), Some("done" | "idle")));
    }
    if scenario == "removed_agent" {
        agents.retain(|agent| agent["pane_id"] != "w1:p1");
    }
    updated
}

fn exercise_live_picker(scenario: &str, key: &[u8], expected: Option<&str>) {
    let mut fixture = Fixture::new(scenario);
    fixture.env("FAKE_MODE", "live");
    let refresh_started = fixture.path("refresh-started");
    let slow = scenario.starts_with("slow_");
    if slow {
        fixture
            .env("SLOW_REFRESH", "1")
            .env("REFRESH_STARTED", &refresh_started);
    }
    let starting = match scenario {
        "initially_empty" => snapshot(&[]),
        "third_done_via_idle" => snapshot(&[
            (1, "done", "First finish"),
            (2, "done", "Second finish"),
            (3, "working", "Third task"),
        ]),
        _ => snapshot(&[
            (1, "working", "Original task"),
            (2, "blocked", "Waiting task"),
            (3, "idle", "Third task"),
        ]),
    };
    fixture.write_json("data.json", &starting);
    let mut picker = fixture.spawn(&[], 24, 120);
    picker.wait_for_timeout(
        if scenario == "initially_empty" {
            b"No running agents"
        } else {
            b"Third task"
        },
        Duration::from_secs(7),
    );

    if slow {
        picker.wait_until(|| refresh_started.exists(), Duration::from_secs(7));
    }

    if slow && scenario != "slow_preserve_cursor" {
        let pressed_at = Instant::now();
        let limit = Duration::from_millis(700);
        picker.send(key);
        picker.wait_until(
            || fixture.focus().is_some(),
            limit.saturating_sub(pressed_at.elapsed()),
        );
        assert!(
            pressed_at.elapsed() <= limit,
            "input was blocked during the 1.5-second snapshot refresh"
        );
        assert_eq!(fixture.focus().as_deref(), expected);
        let exit_started = Instant::now();
        assert!(picker.finish().success(), "{}", picker.text());
        assert!(
            exit_started.elapsed() <= Duration::from_secs(2),
            "picker did not exit during a slow refresh"
        );
        return;
    }

    if matches!(scenario, "preserve_cursor" | "slow_preserve_cursor") {
        picker.send(b"j");
        picker.drain(Duration::from_millis(150));
    }
    if matches!(scenario, "failed_refresh" | "refresh_recovers") {
        fixture.write("data.json", "invalid JSON");
        picker.wait_until(
            || {
                fixture
                    .calls()
                    .iter()
                    .filter(|call| call.as_slice() == ["api", "snapshot"])
                    .count()
                    >= 2
            },
            Duration::from_secs(7),
        );
        picker.drain(Duration::from_millis(200));
        assert_eq!(fixture.focus(), None, "failed refresh changed focus");
    }

    if scenario != "failed_refresh" {
        if scenario == "third_done_via_idle" {
            fixture.seed_event("w1:p3", "working", now() - 1.0);
            fixture.seed_event("w1:p3", "idle", now());
        }
        let completion_age = if scenario == "third_done_via_idle" {
            2.0
        } else {
            0.0
        };
        fixture.seed_event("w1:p1", "done", now() - completion_age);
        fixture.write_json("data.json", &updated_snapshot(scenario));
        picker.clear();
        let marker: &[u8] = match scenario {
            "becomes_empty" => b"No running agents",
            "third_done_via_idle" => b"Third completion",
            "done_only" => b"Finished task",
            _ => b"New blocked task",
        };
        picker.wait_for_timeout(marker, Duration::from_secs(7));
        let output = picker.text();
        if scenario == "third_done_via_idle" {
            assert!(output.matches("[done]").count() >= 3, "{output}");
            assert!(!output.contains("[idle]"), "{output}");
        }
        if !matches!(scenario, "becomes_empty" | "removed_agent") {
            assert!(output.contains("Finished task"), "{output}");
            assert!(output.contains("just now"), "{output}");
            assert!(output.contains("Done"), "{output}");
        }
        assert_eq!(fixture.focus(), None, "refreshing changed focus");
    }
    if scenario == "becomes_empty" {
        picker.send(b"19\r");
        picker.drain(Duration::from_millis(150));
        assert_eq!(fixture.focus(), None, "empty list accepted a selection");
    }
    picker.send(key);
    assert!(picker.finish().success(), "{scenario}\n{}", picker.text());
    assert_eq!(
        fixture.focus().as_deref(),
        expected,
        "{scenario}\n{}",
        picker.text()
    );
    if scenario == "third_done_via_idle" {
        let times = fixture.times();
        assert_eq!(times["w1:p3"]["pending_done"], false);
        assert!(!times["w1:p3"]["completed_at"].is_null());
    }
    if scenario == "becomes_empty" {
        let stopped_calls = fixture.calls();
        std::thread::sleep(Duration::from_millis(1_200));
        assert_eq!(
            fixture.calls(),
            stopped_calls,
            "polling continued after closing"
        );
    }
}

macro_rules! live_cases {
    ($($name:ident: $key:expr, $expected:expr;)*) => {$(
        #[test]
        fn $name() {
            exercise_live_picker(stringify!($name), $key, $expected);
        }
    )*};
}

live_cases! {
    preserve_cursor: b"\r", Some("w1:p3");
    new_agent_shortcut: b"2", Some("w1:p4");
    changed_section_shortcut: b"1", Some("w1:p1");
    done_only: b"1", Some("w1:p1");
    third_done_via_idle: b"1", Some("w1:p3");
    removed_agent: b"1", Some("w1:p4");
    initially_empty: b"2", Some("w1:p4");
    becomes_empty: b"\x1b", None;
    failed_refresh: b"3", Some("w1:p3");
    refresh_recovers: b"2", Some("w1:p4");
    slow_navigation: b"j\r", Some("w1:p3");
    slow_enter: b"\r", Some("w1:p1");
    slow_number: b"3", Some("w1:p3");
    slow_preserve_cursor: b"\r", Some("w1:p3");
}
