//! Real fzf + isolated terminal-attach PTY: byte delivery, identity and cleanup.
mod common;
use common::{alive, until, Fixture, Pty, Screen};
use serde_json::{json, Value};
use std::{fs, time::Duration};
const WAIT: Duration = Duration::from_secs(6);
fn press(pty: &mut Pty, keys: &[u8]) {
    pty.send(keys);
    pty.drain(Duration::from_millis(150));
}
fn received(f: &Fixture) -> Vec<u8> {
    fs::read(f.path("received")).unwrap()
}
fn calls(f: &Fixture, pair: [&str; 2]) -> usize {
    f.calls()
        .iter()
        .filter(|c| c.starts_with(&pair.map(str::to_owned)))
        .count()
}
fn scenario(case: &str) {
    let mut f = Fixture::new(case);
    f.write("received", "");
    f.env("FAKE_MODE", "interactive");
    fs::create_dir_all(f.path("tmp")).unwrap();
    f.env("TMPDIR", f.path("tmp"));
    let mut data = json!({"focused_pane_id":"outside", "panes":[{"pane_id":"outside"}],
        "agents": (1..=2).map(|i| json!({"pane_id":format!("w1:p{i}"),"terminal_id":format!("term-{i}"),
            "agent":"claude", "name":format!("writer{i}"), "agent_status":"working", "focused":i==1,
            "workspace_id":"w1", "tab_id":"w1:t1", "title":format!("Task {i}"),
            "agent_session":{"value":format!("session-{i}")}})).collect::<Vec<_>>()});
    match case {
        "scrolled_entry" => data["long_preview"] = json!(true),
        "malicious_output" => data["malicious"] = json!(true),
        "empty_list" => data["agents"] = json!([]),
        "origin" => data["focused_pane_id"] = json!("w1:p1"),
        "unknown_origin" => data["focused_pane_id"] = Value::Null,
        "shell_on_open" => data["shell"] = json!(true),
        "busy_attach" => {
            f.env("FAIL_ATTACH", "1");
        }
        _ => (),
    }
    f.write_json("data.json", &data);
    let mut p = f.spawn(&[], 24, 160);
    p.wait_for(if case == "empty_list" {
        b"No running agents"
    } else if case == "busy_attach" {
        b"Preview unavailable"
    } else {
        b"LIVE"
    });
    assert!(
        p.text().contains("Task 1") || case == "empty_list",
        "{}",
        p.text()
    );
    match case {
        "search_i" => {
            press(&mut p, b"/");
            press(&mut p, b"NeverMatch-iJKudUD");
            p.wait_for(b"NeverMatch-iJKudUD");
            assert_eq!(calls(&f, ["pane", "process-info"]), 0);
            p.clear();
            p.send(b"\x1b");
            p.wait_for(b"Task 1");
        }
        "empty_list" => {
            press(&mut p, b"i");
            assert_eq!(calls(&f, ["pane", "process-info"]), 0);
        }
        "hidden" | "origin" | "unknown_origin" | "shell_on_open" | "busy_attach" => {
            if case == "hidden" {
                press(&mut p, b"p");
            }
            p.clear();
            press(&mut p, b"i");
            p.wait_for(b"Input stopped:");
            press(&mut p, b"unsubmitted");
            assert!(received(&f).is_empty());
            press(&mut p, b"\x1b");
        }
        _ => exercise(case, &f, &mut p, &mut data),
    }
    assert!(p.alive(), "picker closed: {}", p.text());
    for pair in [
        ["agent", "prompt"],
        ["pane", "send-text"],
        ["pane", "send-keys"],
        ["agent", "send-keys"],
        ["pane", "focus"],
        ["agent", "focus"],
        ["pane", "close"],
    ] {
        assert_eq!(calls(&f, pair), 0, "{}", f.call_text());
    }
    assert!(!f.path("NEVER").exists());
    if case == "killed_picker" {
        p.kill();
    } else if case == "terminated_picker" {
        unsafe {
            libc::kill(p.pid() as i32, libc::SIGTERM);
        }
        assert!(!p.finish().success());
        assert!(
            !p.text().contains("Press Enter to close"),
            "termination waited for input"
        );
    } else {
        press(&mut p, b"q");
        assert!(p.finish().success(), "{}", p.text());
    }
    if let Ok(pid) = fs::read_to_string(f.path("attached")) {
        until(|| !alive(pid.parse().unwrap()), WAIT);
    }
    let count = f.calls().len();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(f.calls().len(), count, "polling after close");
    assert!(
        !f.events().iter().any(|e| e.starts_with("unexpected")),
        "{}",
        f.call_text()
    );
    for event in f.events().iter().filter(|e| e.starts_with("attach ")) {
        let fields: Vec<_> = event.split_whitespace().collect();
        assert_eq!(fields[2], fields[4], "PTY column mismatch: {event}");
        assert_eq!(fields[3], fields[5], "PTY row mismatch: {event}");
        assert_eq!(fields[1], "term-1", "target shifted: {event}");
    }
}
fn exercise(case: &str, f: &Fixture, p: &mut Pty, data: &mut Value) {
    let mut screen = Screen::new(24, 160);
    screen.feed(&p.output);
    if case == "scrolled_entry" {
        let visible = |s: &Screen, text: &str| {
            s.frame
                .iter()
                .any(|row| row[65..].iter().collect::<String>().contains(text))
        };
        p.clear();
        let deadline = std::time::Instant::now() + WAIT;
        while !visible(&screen, "LIVE ready") || !visible(&screen, "PREVIOUS_ROW_11") {
            assert!(
                std::time::Instant::now() < deadline,
                "initial frame did not settle"
            );
            screen.pump(p);
        }
        assert!(!visible(&screen, "PREVIOUS_ROW_10"));
        let mut trace = Vec::new();
        for byte in b"\x1b[1;2A" {
            p.send(&[*byte]);
            p.drain(Duration::from_millis(5));
            trace.extend_from_slice(&p.output);
            screen.feed(&p.output);
            p.clear();
        }
        let deadline = std::time::Instant::now() + WAIT;
        while visible(&screen, "LIVE ready") || !visible(&screen, "PREVIOUS_ROW_10") {
            assert!(
                std::time::Instant::now() < deadline,
                "preview did not scroll; frame: {:?}; events: {:?}; output: {:?}",
                screen
                    .frame
                    .iter()
                    .map(|r| r.iter().collect::<String>())
                    .collect::<Vec<_>>(),
                f.events(),
                String::from_utf8_lossy(&trace)
            );
            p.drain(Duration::from_millis(30));
            trace.extend_from_slice(&p.output);
            screen.feed(&p.output);
            p.clear();
        }
    }
    p.clear();
    press(p, if case == "burst" { b"ix" } else { b"i" });
    p.wait_until(|| calls(f, ["pane", "process-info"]) > 0, WAIT);
    if case == "scrolled_entry" {
        assert!(f
            .calls()
            .iter()
            .any(|call| call == &["pane", "process-info", "--pane", "w1:p1"]));
        assert!(f
            .events()
            .iter()
            .any(|event| event.starts_with("attach term-1 ")));
    }
    p.wait_for(b"Esc returns to picker");
    // A single byte must arrive while no Enter has ever been typed.
    if case != "burst" {
        press(p, b"x");
    }
    p.wait_until(|| received(f) == b"x", WAIT);
    // fzf may emit the text before it commits the frame containing it.
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        screen.feed(&p.output);
        p.clear();
        if screen
            .frame
            .iter()
            .any(|row| row[65..].iter().collect::<String>().contains("LIVE x"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "target echo never painted in the right preview"
        );
        p.drain(Duration::from_millis(20));
    }
    let left = screen
        .frame
        .iter()
        .map(|r| r[..65].iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    let right = screen
        .frame
        .iter()
        .map(|r| r[65..].iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        left.contains("Task 1") && left.contains("Task 2"),
        "left list disappeared: {left}"
    );
    assert!(
        right.contains("LIVE x"),
        "target echo is outside right preview: {right}"
    );

    match case {
        "letters_before_enter" => {
            press(p, b"abc");
            p.wait_until(|| received(f) == b"xabc", WAIT);
            press(p, b"\r");
            p.wait_until(|| received(f) == b"xabc\r", WAIT);
        }
        "specials" => {
            let bytes = b"\x1b[A\x1b[B\x1b[C\x1b[D\x7f\x08\t\x03\x04\x00\xff";
            press(p, bytes);
            p.wait_until(
                || received(f) == [b"x".as_slice(), bytes.as_slice()].concat(),
                WAIT,
            );
        }
        "utf8_fragmented" => {
            for byte in "한글".bytes() {
                let mut want = received(f);
                want.push(byte);
                press(p, &[byte]);
                p.wait_until(|| received(f) == want, WAIT);
            }
        }
        "bracketed_paste" => {
            let bytes = b"\x1b[200~first\r\n\x1b\x00\x03\xff'$(touch NEVER)'\nlast\x1b[201~";
            // Send framing in pieces and an isolated ESC inside the payload.
            press(p, &bytes[..6]);
            for byte in &bytes[6..bytes.len() - 6] {
                press(p, &[*byte]);
            }
            press(p, &bytes[bytes.len() - 6..]);
            p.wait_until(
                || received(f) == [b"x".as_slice(), bytes.as_slice()].concat(),
                WAIT,
            );
        }
        "delayed" => {
            p.drain(Duration::from_millis(1500));
            press(p, "지금\x1b[D\x7f".as_bytes());
            p.wait_until(|| received(f) == "x지금\x1b[D\x7f".as_bytes(), WAIT);
        }
        "reorder" => {
            data["agents"].as_array_mut().unwrap().reverse();
            data["agents"][0]["focused"] = json!(true);
            data["agents"][0]["title"] = json!("Reordered second");
            f.write_json("data.json", data);
            p.drain(Duration::from_millis(1300));
            press(p, b"jkJKudUD2q\r");
            p.wait_until(|| received(f) == b"xjkJKudUD2q\r", WAIT);
        }
        "resize" => {
            p.resize(30, 180);
            p.drain(Duration::from_millis(400));
            // Resize must end input, never forward queued input to a new attach.
            p.wait_for(b"Input stopped:");
        }
        "gone" | "replaced" | "session_replaced" | "terminal_replaced" | "retained_shell"
        | "lookup_error" | "attach_ended" | "read_failure" => {
            match case {
                "gone" => {
                    data["agents"].as_array_mut().unwrap().remove(0);
                }
                "replaced" => data["pid"] = json!(999),
                "session_replaced" => {
                    data["agents"][0]["agent_session"]["value"] = json!("new-session")
                }
                "terminal_replaced" => data["agents"][0]["terminal_id"] = json!("new-terminal"),
                "retained_shell" => data["shell"] = json!(true),
                "lookup_error" => data["get_error"] = json!(true),
                "attach_ended" => data["end_attach"] = json!(true),
                "read_failure" => {
                    f.write("response", "unused");
                    data["read_error"] = json!(true);
                }
                _ => unreachable!(),
            }
            f.write_json("data.json", data);
            p.wait_for(b"Input stopped:");
            press(p, b"blocked\r");
            assert_eq!(received(f), b"x", "input leaked after identity/end failure");
        }
        "escape" | "reenter" | "burst" | "killed_picker" | "terminated_picker"
        | "scrolled_entry" => (),
        "malicious_output" => {
            assert!(
                !left.contains("LEFT_CORRUPTED"),
                "target moved cursor across boundary"
            );
            for code in ["\x1b]52;", "\x1bP", "\x1b[?1049h"] {
                assert!(
                    !p.text().contains(code),
                    "unsafe attachment/snapshot replay: {code:?}"
                );
            }
        }
        _ => panic!("unknown case"),
    }
    if matches!(case, "killed_picker" | "terminated_picker") {
        return;
    }
    let expected = received(f);
    press(p, b"\x1b");
    assert_eq!(received(f), expected, "standalone escape reached target");
    if case == "reenter" {
        let before = calls(f, ["pane", "process-info"]);
        press(p, b"i");
        p.wait_until(|| calls(f, ["pane", "process-info"]) > before, WAIT);
        press(p, b"z");
        p.wait_until(|| received(f) == b"xz", WAIT);
        press(p, b"\x1b");
    }
}
macro_rules! cases { ($($name:ident),+ $(,)?) => { $(#[test] fn $name() { scenario(stringify!($name)); })+ }; }
cases!(
    scrolled_entry,
    burst,
    malicious_output,
    killed_picker,
    terminated_picker,
    letters_before_enter,
    specials,
    utf8_fragmented,
    bracketed_paste,
    delayed,
    reorder,
    escape,
    reenter,
    gone,
    replaced,
    session_replaced,
    terminal_replaced,
    retained_shell,
    lookup_error,
    attach_ended,
    read_failure,
    search_i,
    empty_list,
    origin,
    unknown_origin,
    shell_on_open,
    busy_attach,
    hidden,
    resize
);
