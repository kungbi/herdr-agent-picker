mod common;

use common::{local_format, local_time, now, output, plain, sql, styles, timestamp, Fixture};
use regex::Regex;
use std::{collections::HashSet, process::Command, time::Duration};

const READ_PREVIEW: &[&str] = &[
    "pane",
    "read",
    "w1:p1",
    "--source",
    "recent-unwrapped",
    "--lines",
    "30",
    "--format",
    "ansi",
];

fn days_ago_at_noon(at: f64, days: i32) -> f64 {
    let mut tm = local_time(at);
    tm.tm_mday -= days;
    tm.tm_hour = 12;
    tm.tm_min = 0;
    tm.tm_sec = 0;
    timestamp(tm)
}

fn local_date(timestamp: f64) -> String {
    local_format(timestamp, "%F")
}

fn sgr() -> Regex {
    Regex::new(r"\x1b\[[0-9;:]*m").unwrap()
}

fn assert_original_preview(kind: &str) {
    let mut fixture = Fixture::new(kind);
    fixture
        .env("FAKE_MODE", "picker")
        .env("PICKER_AGENT_KIND", kind);
    let output = fixture.run(&["--preview", "w1:p1"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"\x1b[38;2;17;203;91mPREVIEW for w1:p1\x1b[0m\n"
    );
    assert_eq!(fixture.calls(), [READ_PREVIEW]);
}

#[test]
fn preview_preserves_ansi_and_newline() {
    assert_original_preview("claude");
}

#[test]
fn codex_reads_original_terminal_snapshot() {
    assert_original_preview("codex");
}

#[test]
fn hermes_reads_original_terminal_snapshot() {
    assert_original_preview("hermes");
}

#[test]
fn padded_preview_preserves_styles_and_indentation() {
    let mut fixture = Fixture::new("padded-output");
    fixture
        .env("FAKE_MODE", "picker")
        .env("PICKER_PADDED_PREVIEW", "1");
    let padded = fixture.run(&["--preview", "w1:p1"]);
    let mut fake = fixture.configure(Command::new(fixture.path("herdr")));
    fake.args(["pane", "read", "w1:p1", "--format", "ansi"]);
    let original = output(fake, Duration::from_secs(5));
    assert!(padded.status.success(), "{padded:?}");
    assert!(original.status.success(), "{original:?}");
    let padded = String::from_utf8(padded.stdout).unwrap();
    let original = String::from_utf8(original.stdout).unwrap();
    assert_eq!(styles(&padded), styles(&original));
    assert_eq!(
        sgr().replace_all(&padded, ""),
        concat!(
            "PREVIEW for w1:p1\n    ROW 0\n    ROW 1\n    ROW 2\n\n",
            "    ROW 4\n    ROW 5\n    ROW 6\n    ROW 7\n",
            "    한글 들여쓰기\nliteral │\nliteral ┃\nPADDED_END\n",
        ),
    );
}

fn assert_sections(output: &str, count: usize, opened: f64, history: f64) {
    assert!(output.contains("3m ago"), "{output}");
    let history_date = local_date(history);
    let headings = [opened, now()].map(|observed| {
        if local_date(observed) == history_date {
            "Today"
        } else if local_date(days_ago_at_noon(observed, 1)) == history_date {
            "Yesterday"
        } else {
            &history_date
        }
    });
    let history_heading = headings
        .into_iter()
        .find(|heading| output.contains(heading))
        .unwrap_or_else(|| panic!("missing history section {headings:?}\n{output}"));
    for heading in ["Active", "Done", "Other"] {
        assert!(output.contains(heading), "missing {heading}\n{output}");
    }
    assert!(!output.contains("gpt-6.1-sol"), "{output}");
    let states = ["working", "blocked", "done", "idle", "unknown"];
    for state in states {
        assert!(
            output.contains(&format!("[{state}]")),
            "missing {state}\n{output}"
        );
    }
    assert!(
        output.contains("38;"),
        "missing foreground colors\n{output}"
    );
    let colors =
        Regex::new(r"\x1b\[([0-9;]*)m[^\x1b]*\[(working|blocked|done|idle|unknown)\]").unwrap();
    let by_state: std::collections::HashMap<_, _> = colors
        .captures_iter(output)
        .map(|capture| {
            (
                capture.get(2).unwrap().as_str(),
                capture.get(1).unwrap().as_str(),
            )
        })
        .collect();
    assert_eq!(
        by_state.values().collect::<HashSet<_>>().len(),
        states.len(),
        "{by_state:?}"
    );
    let plain = sgr().replace_all(output, "");
    assert!(
        plain.find("Done").unwrap() < plain.find("Active").unwrap(),
        "{plain}"
    );
    assert!(
        plain.find("Active").unwrap() < plain.find(history_heading).unwrap(),
        "{plain}"
    );
    assert!(
        plain.find("[working]").unwrap() < plain.find("[idle]").unwrap(),
        "{plain}"
    );
    let numbers: Vec<usize> = Regex::new(r"\[(\d+)\]")
        .unwrap()
        .captures_iter(&plain)
        .map(|capture| capture[1].parse().unwrap())
        .collect();
    assert_eq!(numbers, (1..=count.min(9)).collect::<Vec<_>>(), "{plain}");
}

fn exercise_picker(name: &str, count: usize, keys: &[&[u8]], expected: Option<&str>) {
    let mut fixture = Fixture::new(name);
    fixture
        .env("FAKE_MODE", "picker")
        .env("PICKER_AGENT_COUNT", count.to_string());
    let alarm = name.starts_with("alarm_unread_");
    let scope = sql(fixture.context()["scope"].as_str().unwrap());
    let unread_sql = format!("SELECT pane_id FROM unread_reminders WHERE scope={scope}");
    if alarm {
        fixture.env("PICKER_UNREAD_ALARM", "1");
        fixture.db("CREATE TABLE IF NOT EXISTS unread_reminders (scope TEXT NOT NULL, pane_id TEXT NOT NULL, PRIMARY KEY (scope, pane_id));");
        fixture.db(&format!(
            "INSERT INTO unread_reminders (scope, pane_id) VALUES ({scope}, 'w1:p1');"
        ));
    }
    if name == "preview_long_line" {
        fixture.env("PICKER_LONG_PREVIEW", "1");
    }
    if name == "preview_padded" {
        fixture.env("PICKER_PADDED_PREVIEW", "1");
    }
    let opened = now();
    let history = days_ago_at_noon(opened, 1);
    fixture.seed_event("w1:p3", "done", opened - 180.0);
    fixture.seed_event("w1:p4", "done", history);
    fixture.seed_event("w1:p4", "idle", history + 1.0);
    let mut picker = fixture.spawn(&[], 24, if alarm { 72 } else { 100 });
    picker.wait_for(if count <= 12 { b"Agent 5" } else { b"Agent 10" });
    picker.wait_for(b"PREVIEW for w1:p1");
    let output = picker.text();
    if alarm {
        assert!(output.contains("[!]"), "unread marker clipped\n{output}");
        let red = Regex::new(r"\x1b\[[0-9;]*38;2;255;85;85(?:;[0-9;]*)?m\[!\]").unwrap();
        assert!(
            red.is_match(&output),
            "unread marker lost its red color\n{output}"
        );
        assert_eq!(fixture.db(&unread_sql), [["w1:p1"]]);
    }
    let calls = fixture.calls();
    let data_calls = calls
        .iter()
        .filter(|call| {
            !matches!(
                (call[0].as_str(), call[1].as_str()),
                ("pane", "read" | "get") | ("terminal", "attach") | ("agent", "get")
            )
        })
        .collect::<Vec<_>>();
    assert!(
        data_calls
            .iter()
            .map(|call| call.join(" "))
            .eq(["api snapshot"])
            || data_calls.iter().map(|call| call.join(" ")).eq([
                "agent list",
                "workspace list",
                "tab list"
            ]),
        "{calls:?}",
    );
    assert!(calls.iter().any(|call| call == READ_PREVIEW), "{calls:?}");
    assert_eq!(fixture.focus(), None, "opening the picker changed focus");
    if count <= 12 {
        assert_sections(&output, count, opened, history);
    }

    let search = name.starts_with("search_");
    let (last, intermediate) = keys.split_last().unwrap();
    for keys in intermediate {
        let opening_search = search && *keys == b"/";
        if opening_search {
            picker.drain(Duration::from_millis(200));
            picker.clear();
        }
        picker.send(keys);
        picker.drain(Duration::from_millis(if search { 600 } else { 150 }));
        assert!(picker.alive(), "{name} closed before the final action");
        if opening_search {
            picker.drain(Duration::from_millis(200));
            assert!(!picker.text().contains("!^"), "{}", picker.text());
        }
        assert_eq!(
            fixture.focus(),
            None,
            "partial input or navigation changed focus"
        );
    }

    if matches!(
        name,
        "preview_default"
            | "preview_move"
            | "preview_toggle"
            | "preview_long_line"
            | "preview_padded"
            | "search_escape_preview"
    ) {
        picker.wait_for_timeout(
            format!("PREVIEW for {}", expected.unwrap()).as_bytes(),
            Duration::from_secs(3),
        );
        let output = picker.text();
        let green = Regex::new(r"\x1b\[[0-9;]*38;2;17;203;91(?:;[0-9;]*)?m").unwrap();
        assert!(green.is_match(&output), "preview color was lost\n{output}");
        assert!(
            !plain(&output).contains("[38;2;17;203;91m"),
            "visible ANSI fragments\n{output}"
        );
    }
    match name {
        "preview_long_line" => {
            picker.wait_for_timeout(b"END_OF_PREVIEW", Duration::from_secs(3));
            assert!(picker.text().contains("LONG_PREVIEW"));
        }
        "preview_hidden" => {
            let calls = fixture.call_text();
            assert!(
                !calls
                    .lines()
                    .any(|call| call.starts_with("pane read w1:p4 ")),
                "{calls}"
            );
        }
        "preview_padded" => picker.wait_for_timeout(b"PADDED_END", Duration::from_secs(3)),
        _ => {}
    }
    picker.send(last);
    assert!(picker.finish().success(), "{name}\n{}", picker.text());
    assert_eq!(
        fixture.focus().as_deref(),
        expected,
        "{name}\n{}",
        picker.text()
    );
    if alarm {
        let unread: Vec<Vec<String>> = if expected.is_none() {
            vec![vec!["w1:p1".into()]]
        } else {
            vec![]
        };
        assert_eq!(fixture.db(&unread_sql), unread);
    }
}

macro_rules! picker_cases {
    ($($name:ident: $count:literal, [$($keys:expr),+], $expected:expr;)*) => {$(
        #[test]
        fn $name() {
            exercise_picker(stringify!($name), $count, &[$($keys),+], $expected);
        }
    )*};
}

picker_cases! {
    alarm_unread_cancel: 5, [b"q"], None;
    alarm_unread_open: 5, [b"\r"], Some("w1:p1");
    preview_padded: 5, [b"\r"], Some("w1:p1");
    quit: 5, [b"q"], None;
    search_q_text: 5, [b"/", b"Agent2q", b"\r", b"\x7f", b"\r"], Some("w1:p2");
    search_escape_quit: 5, [b"/", b"q", b"\x1b", b"q"], None;
    // A single fuzzy term avoids matching "2" against a clock-dependent age.
    search_title: 5, [b"/", b"Agent2", b"\r"], Some("w1:p2");
    search_workspace: 5, [b"/", b"Test workspace", b"\r"], Some("w1:p1");
    search_agent_name: 5, [b"/", b"claude", b"\r"], Some("w1:p1");
    search_escape_empty: 5, [b"/", b"\x1b", b"\x1b"], None;
    search_escape_text: 5, [b"/", b"Agent2", b"\x1b", b"\x1b"], None;
    search_escape_number: 5, [b"/", b"Agent2", b"\x1b", b"1"], Some("w1:p3");
    search_escape_no_match: 5, [b"/", b"No such agent 987", b"\x1b", b"1"], Some("w1:p3");
    search_escape_navigation: 5, [b"/", b"Agent2", b"\x1b", b"j", b"\r"], Some("w1:p1");
    search_escape_preview: 5, [b"/", b"Agent2", b"\x1b", b"p", b"p", b"\r"], Some("w1:p2");
    search_escape_reenter: 5, [b"/", b"Agent2", b"\x1b", b"/", b"Agent4", b"\r"], Some("w1:p4");
    preview_default: 5, [b"\r"], Some("w1:p1");
    preview_move: 5, [b"j", b"\r"], Some("w1:p4");
    preview_toggle: 5, [b"p", b"p", b"\r"], Some("w1:p1");
    preview_hidden: 5, [b"p", b"j", b"\r"], Some("w1:p4");
    preview_long_line: 5, [b"\r"], Some("w1:p1");
    select: 5, [b"jkj", b"\r"], Some("w1:p4");
    cancel: 5, [b"jkj", b"\x1b"], None;
    cross_up: 5, [b"jk", b"\r"], Some("w1:p1");
    arrows: 5, [b"\x1b[B\x1b[A\x1b[B", b"\r"], Some("w1:p4");
    number_first: 5, [b"1"], Some("w1:p3");
    number_date_section: 5, [b"4"], Some("w1:p4");
    number_last: 5, [b"5"], Some("w1:p5");
    number_missing: 5, [b"09", b"3"], Some("w1:p1");
    number_first_large_list: 12, [b"1"], Some("w1:p3");
    number_ninth: 12, [b"9"], Some("w1:p10");
    number_zero_ignored: 12, [b"0", b"9"], Some("w1:p10");
    navigate_tenth: 12, [b"jjjjjjj", b"\r"], Some("w1:p11");
    navigate_last: 12, [b"jjjjjjjjj", b"\r"], Some("w1:p5");
    jump_down: 40, [b"\x04", b"\r"], Some("w1:p9");
    jump_up: 40, [b"\x04\x15", b"\r"], Some("w1:p1");
    jump_small_down: 5, [b"\x04", b"\r"], Some("w1:p5");
    jump_small_up: 5, [b"\x15", b"\r"], Some("w1:p3");
    jump_up_top: 40, [b"\x15", b"\r"], Some("w1:p3");
    jump_down_end: 40, [&[4; 8], b"\r"], Some("w1:p5");
    jump_up_end: 40, [b"\x04\x04\x04\x04\x04\x04\x04\x04\x15", b"\r"], Some("w1:p36");
}
