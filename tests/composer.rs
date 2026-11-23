mod common;

use common::{Fixture, Pty};
use serde_json::{json, Value};
use std::time::Duration;

const ESC: &[u8] = b"\x1b";
const PAYLOAD: &str = r#"  --한글 'quote' "double" $HOME $(touch NEVER) `cmd` ; | & \ 끝  "#;

fn press(pty: &mut Pty, keys: &[u8]) {
    pty.send(keys);
    pty.drain(Duration::from_millis(200));
}

fn calls_for(fixture: &Fixture, command: [&str; 2]) -> Vec<Vec<String>> {
    fixture
        .calls()
        .into_iter()
        .filter(|args| args.starts_with(&command.map(str::to_owned)))
        .collect()
}

fn assert_picker_alive(pty: &mut Pty, fixture: &Fixture) {
    assert!(pty.alive(), "picker closed: {}", pty.text());
    for command in [["agent", "focus"], ["pane", "focus"], ["pane", "close"]] {
        assert!(
            calls_for(fixture, command).is_empty(),
            "{}",
            fixture.call_text()
        );
    }
}

fn snapshot() -> Value {
    let agents: Vec<_> = (1..=2).map(|i| json!({
        "pane_id": format!("w1:p{i}"), "terminal_id": format!("term-{i}"),
        "agent": "claude", "name": format!("writer{i}"), "agent_status": "working",
        "focused": i == 1, "workspace_id": "w1", "tab_id": "w1:t1",
        "title": format!("Task {i}"),
        "agent_session": {"agent": "claude", "kind": "id", "source": "hook", "value": format!("session-{i}")}
    })).collect();
    json!({"agents": agents, "workspaces": [], "tabs": []})
}

fn composer(case: &str) {
    let mut fixture = Fixture::new(case);
    fixture.env("FAKE_MODE", "composer");
    fixture.write("config.toml", "[ui]\nagent_panel_sort = \"priority\"\n");
    let mut data = snapshot();
    if case == "empty_list" {
        data["agents"] = json!([]);
    }
    if case == "shell_on_open" {
        data["shell"] = json!(true);
    }
    fixture.write_json("data.json", &data);
    let mut pty = fixture.spawn(&[], 24, 160);
    pty.wait_for(if case == "empty_list" {
        b"No running agents"
    } else {
        b"PREVIEW for w1:p1"
    });

    match case {
        "search_i" => {
            press(&mut pty, b"/");
            pty.clear();
            press(&mut pty, b"NeverMatch-i");
            pty.wait_for(b"NeverMatch-i");
            assert!(!pty.text().contains("To w1:"));
            assert!(calls_for(&fixture, ["pane", "process-info"]).is_empty());
            press(&mut pty, ESC);
        }
        "empty_list" => {
            press(&mut pty, b"i");
            assert!(!pty.text().contains("To "));
        }
        "shell_on_open" => {
            pty.clear();
            press(&mut pty, b"i");
            pty.wait_for(b"Nothing sent");
            press(&mut pty, ESC);
        }
        _ => compose(case, &fixture, &mut pty, &mut data),
    }

    assert_picker_alive(&mut pty, &fixture);
    assert!(
        !fixture.path("NEVER").exists(),
        "payload was evaluated by a shell"
    );
    press(&mut pty, b"q");
    assert!(pty.finish().success(), "{}", pty.text());
    for command in [
        ["agent", "focus"],
        ["pane", "send-text"],
        ["pane", "send-keys"],
        ["agent", "send-keys"],
        ["pane", "close"],
    ] {
        assert!(
            calls_for(&fixture, command).is_empty(),
            "{}",
            fixture.call_text()
        );
    }
}

fn compose(case: &str, fixture: &Fixture, pty: &mut Pty, data: &mut Value) {
    let prompts = || calls_for(fixture, ["agent", "prompt"]);
    pty.clear();
    press(pty, b"i");
    pty.wait_for(b"To w1:p1 / pid 101 / writer1 (claude)");
    assert!(
        pty.text().contains("\x1b[24;1H"),
        "composer is not on the bottom row"
    );
    let identity_changed = match case {
        "gone" => {
            data["agents"].as_array_mut().unwrap().remove(0);
            true
        }
        "replaced" => {
            data["pid"] = json!(999);
            true
        }
        "session_replaced" => {
            data["agents"][0]["agent_session"]["value"] = json!("new-session");
            true
        }
        "terminal_replaced" => {
            data["agents"][0]["terminal_id"] = json!("new-terminal");
            true
        }
        "retained_shell" => {
            data["shell"] = json!(true);
            true
        }
        "lookup_error" => {
            data["get_error"] = json!(true);
            true
        }
        "reorder" => {
            data["agents"].as_array_mut().unwrap().reverse();
            data["agents"][0]["agent_status"] = json!("done");
            data["agents"][0]["focused"] = json!(true);
            data["agents"][0]["title"] = json!("Reordered second agent");
            data["agents"][1]["focused"] = json!(false);
            true
        }
        _ => false,
    };
    if identity_changed {
        fixture.write_json("data.json", data);
        let previews = || calls_for(fixture, ["pane", "read"]).len();
        let before = previews();
        // Cross fzf's one-second cooked-mode restoration while composing.
        pty.drain(Duration::from_millis(1400));
        assert!(previews() > before, "preview stopped during composition");
        assert!(
            !pty.text().contains("To w1:p2"),
            "recipient shifted with the list"
        );
    }
    match case {
        "partial_error" => data["send_error"] = json!(true),
        "timeout" => data["timeout"] = json!(true),
        _ => {}
    }
    fixture.write_json("data.json", data);

    match case {
        "cancel" => {
            press(pty, PAYLOAD.as_bytes());
            press(pty, ESC);
        }
        "empty"
        | "unbracketed_multiline"
        | "paste_multiline"
        | "paste_control"
        | "paste_invalid_utf8" => {
            match case {
                "empty" => press(pty, b"\r"),
                "unbracketed_multiline" => press(pty, b"first\rsecond\r"),
                _ => {
                    let pasted: &[u8] = match case {
                        "paste_multiline" => b"line1\r\nline2",
                        "paste_control" => b"x\x1b[31m\x00\t\x03y",
                        "paste_invalid_utf8" => b"bad\xff",
                        _ => unreachable!(),
                    };
                    press(pty, &bracketed(pasted));
                    press(pty, b"\r");
                }
            }
            assert!(prompts().is_empty(), "rejected input was submitted");
            assert_picker_alive(pty, fixture);
            press(pty, ESC);
        }
        _ => {
            let payload = match case {
                "paste" => {
                    press(pty, &bracketed(PAYLOAD.as_bytes()));
                    PAYLOAD
                }
                "utf8_edit" => {
                    for byte in "한글".bytes() {
                        press(pty, &[byte]);
                    }
                    press(pty, b"\x7f");
                    press(pty, "끝".as_bytes());
                    "한끝"
                }
                _ => {
                    press(pty, PAYLOAD.as_bytes());
                    PAYLOAD
                }
            };
            press(pty, b"\r");
            if (identity_changed && case != "reorder")
                || matches!(case, "partial_error" | "timeout")
            {
                pty.wait_for(b"Error:");
                if matches!(case, "partial_error" | "timeout") {
                    assert!(pty.text().contains("not retried"));
                } else {
                    assert!(prompts().is_empty(), "{:?}", prompts());
                }
                press(pty, ESC);
            } else {
                pty.wait_until(|| prompts().len() == 1, Duration::from_secs(7));
                assert_eq!(prompts(), [["agent", "prompt", "w1:p1", "--", payload]]);
            }
        }
    }
    pty.drain(Duration::from_millis(300));
    assert_picker_alive(pty, fixture);
    let expected = usize::from(matches!(
        case,
        "literal" | "paste" | "utf8_edit" | "reorder" | "partial_error" | "timeout"
    ));
    assert_eq!(prompts().len(), expected, "{:?}", prompts());
    if case == "reorder" {
        pty.wait_for(b"Reordered second agent");
    }
    if matches!(case, "literal" | "cancel" | "paste" | "reorder") {
        pty.clear();
        press(pty, b"i");
        pty.wait_for(b"To w1:p1");
        press(pty, ESC);
    }
}

fn bracketed(payload: &[u8]) -> Vec<u8> {
    [b"\x1b[200~", payload, b"\x1b[201~"].concat()
}

macro_rules! scenarios {
    ($($case:ident),+ $(,)?) => { $(
        #[test]
        fn $case() {
            composer(stringify!($case));
        }
    )+ };
}

scenarios!(
    cancel,
    empty,
    literal,
    paste,
    paste_multiline,
    paste_control,
    paste_invalid_utf8,
    unbracketed_multiline,
    utf8_edit,
    reorder,
    gone,
    replaced,
    session_replaced,
    terminal_replaced,
    retained_shell,
    lookup_error,
    partial_error,
    timeout,
    search_i,
    empty_list,
    shell_on_open,
);
