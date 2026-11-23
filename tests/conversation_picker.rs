mod common;

use common::Fixture;
use regex::Regex;
use serde_json::json;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

fn framed_snapshot(text: &str, columns: usize, styles: &Regex) -> String {
    text.lines()
        .enumerate()
        .map(|(row, line)| {
            let unstyled = styles.replace_all(line, "");
            let width = UnicodeWidthStr::width(unstyled.as_ref());
            assert!(width < columns);
            let border = if row % 2 == 0 { '┃' } else { '│' };
            format!(
                "{line}{}\x1b[0m\x1b[38;2;22;142;142m{border}\x1b[0m\n",
                " ".repeat(columns - width - 1)
            )
        })
        .collect()
}

fn assert_rendering(fixture: &Fixture, pane: &str, source: &str, expected: &str, styles: &Regex) {
    let output = fixture.run(&["--preview", pane]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fixture.calls().last().unwrap(),
        &[
            "pane",
            "read",
            pane,
            "--source",
            "recent-unwrapped",
            "--lines",
            "30",
            "--format",
            "ansi"
        ]
    );
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        styles.replace_all(&rendered, ""),
        styles.replace_all(expected, "")
    );
    assert_eq!(
        styles
            .find_iter(&rendered)
            .map(|m| m.as_str())
            .collect::<Vec<_>>(),
        styles
            .find_iter(source)
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(rendered.lines().count(), source.lines().count());
}

fn setup() -> Fixture {
    let mut fixture = Fixture::new("conversation");
    fixture.write_json(
        "data",
        &json!({
            "agents": [
                {"agent": "hermes", "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
                 "agent_status": "working", "title": "Fixture 1", "focused": true},
                {"agent": "codex", "pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t1",
                 "agent_status": "working", "title": "Fixture 2", "focused": false}
            ],
            "workspaces": [{"workspace_id": "w1", "label": "Fixtures"}],
            "tabs": [{"tab_id": "w1:t1", "label": "Test"}]
        }),
    );
    let data = fixture.path("data");
    let snapshots = fixture.path("snapshots");
    fixture
        .env("FAKE_MODE", "conversation")
        .env("COLORTERM", "truecolor")
        .env("DATA", data)
        .env("SNAPSHOTS", snapshots);
    fixture
}

fn publish(fixture: &Fixture, first: &str, second: &str) {
    fixture.write_json("snapshots", &json!({"w1:p1": first, "w1:p2": second}));
}

#[test]
fn removes_snapshot_frames_and_padding_without_losing_styles() {
    let fixture = setup();
    let styles = Regex::new(r"\x1b\[[0-9;:]*m").unwrap();
    let literal = "    한글 e\u{301} **CODE_LITERAL** │";
    let width = UnicodeWidthStr::width(literal);
    let expected = format!("{literal}\n\n");
    for padding in [0, 1, 2, 8, 31, 32] {
        let source = framed_snapshot(&expected, width + padding + 1, &styles);
        publish(&fixture, &source, "SECOND_RESPONSE\n");
        assert_rendering(&fixture, "w1:p1", &source, &expected, &styles);
    }

    let table = format!("│ value{}│\n    literal ┃\n", " ".repeat(40));
    publish(&fixture, &table, "SECOND_RESPONSE\n");
    assert_rendering(&fixture, "w1:p1", &table, &table, &styles);

    let source = "    code  \x1b[0m  \x1b[31m \x1b[0m\n";
    publish(&fixture, source, "SECOND_RESPONSE\n");
    assert_rendering(&fixture, "w1:p1", source, "    code\n", &styles);
}

fn native_conversation(columns: u16) {
    let fixture = setup();
    let styles = Regex::new(r"\x1b\[[0-9;:]*m").unwrap();
    let wrap = "한글 미리보기 ".repeat(6);
    let first = format!(
        "\x1b[1;38;2;139;233;253mFIRST_RESPONSE\x1b[0m\n\n\
         \x1b[1m“응답 전송은 에이전트가 담당”\x1b[0m는 설명\n\
         \x1b[38;2;241;250;140m    value = \"**CODE_LITERAL**\"\x1b[0m\n\
         │ Name │ Status │\n│ task │ ready  │\n{}\nWRAP_END\n",
        wrap.trim_end()
    );
    let second = "SECOND_RESPONSE\n";
    let framed = framed_snapshot(&first, 96, &styles);

    publish(&fixture, &framed, second);
    assert_rendering(&fixture, "w1:p1", &framed, &first, &styles);
    assert_rendering(&fixture, "w1:p2", second, second, &styles);

    let mut picker = fixture.spawn(&[], 28, columns);
    picker.wait_for(b"WRAP_END");
    let output = picker.text();
    let visible = common::plain(&output);
    assert!(visible.contains("FIRST_RESPONSE"), "{visible}");
    assert!(visible.contains("**CODE_LITERAL**"), "{visible}");
    assert!(output.contains("한글"), "{output}");
    let truecolor = Regex::new(r"\x1b\[[0-9;]*38;2;139;233;253(?:;[0-9;]*)?m").unwrap();
    assert!(truecolor.is_match(&output), "{output}");
    assert!(!output.contains("Saved conversation") && !output.contains("ASSISTANT"));
    assert!(fixture.focus().is_none());

    if columns == 100 {
        publish(&fixture, "UPDATED_NATIVE_SCREEN\n", second);
        picker.clear();
        picker.wait_for(b"UPDATED_NATIVE_SCREEN");
        picker.clear();
        picker.send(b"j");
        picker.wait_for(b"SECOND_RESPONSE");
        picker.send(b"p");
        picker.drain(Duration::from_millis(500));
        let reads = || {
            fixture
                .calls()
                .iter()
                .filter(|args| args.starts_with(&["pane".into(), "read".into()]))
                .count()
        };
        let before = reads();
        picker.drain(Duration::from_millis(2100));
        assert_eq!(reads(), before, "Hidden preview continued reading panes");

        publish(&fixture, &framed, "AFTER_HIDDEN\n");
        picker.clear();
        picker.send(b"p");
        picker.wait_for(b"AFTER_HIDDEN");
        picker.resize(28, 72);
        picker.drain(Duration::from_millis(300));
        assert!(picker.alive());
        publish(&fixture, &framed, second);
    }

    picker.send(b"q");
    assert!(picker.finish().success());
    assert!(fixture.focus().is_none(), "Preview changed focus");
    let calls = fixture.calls();
    assert!(
        calls.iter().all(|args| match args.as_slice() {
            [group, command] => group == "api" && command == "snapshot",
            [group, command, ..] => matches!(
                (group.as_str(), command.as_str()),
                ("pane", "read" | "get") | ("terminal", "attach")
            ),
            _ => false,
        }),
        "{calls:?}"
    );
}

#[test]
fn conversation_at_72_columns() {
    native_conversation(72);
}

#[test]
fn conversation_at_100_columns() {
    native_conversation(100);
}

#[test]
fn conversation_at_160_columns() {
    native_conversation(160);
}
