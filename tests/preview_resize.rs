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
            Some(("pane", "read")) => {
                assert_eq!(call[3..6], ["--source", "recent-unwrapped", "--lines"]);
                assert!(call[6].parse::<u16>().unwrap() >= 30, "{call:?}");
                assert_eq!(call[7], "--format");
                assert!(matches!(call[8].as_str(), "ansi" | "text"), "{call:?}");
            }
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

fn resize_and_wait(
    fixture: &Fixture,
    picker: &mut Pty,
    screen: &mut Screen,
    cols: u16,
    expected: &[String],
) {
    picker.resize(48, cols);
    let cols = usize::from(cols);
    screen.resize(48, cols);
    let deadline = Instant::now() + WAIT;
    loop {
        screen.pump(picker);
        if screen.frame[0][cols - 1] == '╮' && screen.preview() == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "preview {:?}, Herdr events {:?}",
            screen.preview(),
            fixture.events()
        );
    }
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
    for edit_tail in [false, true] {
        let edited: String = (5..=34)
            .map(|row| {
                let suffix = if row == 24 || (edit_tail && row == 34) {
                    "_EDIT"
                } else {
                    ""
                };
                format!("\x1b[32mROW_{row:02}{suffix}\x1b[0m\n")
            })
            .collect();
        fixture.write("content", &edited);
        assert_stable(&fixture, &mut picker, &mut screen, &scrolled, 2);
    }

    picker.send(b"\x1b[F");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        has(rows, "ROW_34_EDIT") && has(rows, "ROW_24_EDIT")
    });
    assert!(has(&screen.preview(), "ROW_24_EDIT"));
    assert!(!has(&screen.preview(), "ROW_24"));
    assert!(!has(&screen.preview(), "ROW_34"));
    publish_rows(&fixture, 6, 35, "");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        has(rows, "ROW_35")
    });
    publish_rows(&fixture, 1, 30, "_EDIT");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        has(rows, "ROW_30_EDIT")
    });
    assert!(screen.preview().iter().all(|row| row.ends_with("_EDIT")));
    fixture.write("content", "ROW_99\n");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows == ["ROW_99"]
    });
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
fn working_agent_redraw_does_not_repeat_history() {
    let mut f = setup("working-redraw");
    f.env("CONTENT", f.path("content"));
    f.write_json("snapshot.json", &json!({"focused_pane_id":"p1","panes":[{"pane_id":"p1"}],"agents":[agent("p1","Agent",true)],"workspaces":[],"tabs":[]}));
    let publish = |tick| {
        let mut text: String = (1..=24).map(|n| format!("ROW_{n:02}\n")).collect();
        text.push_str(&format!("ROW_{:02}\n", 25 + tick));
        f.write("content", &text);
    };
    publish(0);
    let mut picker = f.spawn(&["picker"], 18, 100);
    let mut screen = Screen::new(18, 100);
    for tick in 0..=3 {
        publish(tick);
        let last = format!("ROW_{:02}", 25 + tick);
        wait_screen(&f, &mut picker, &mut screen, |rows| {
            rows.last() == Some(&last)
        });
    }
    picker.send(b"UUUUUU");
    // At the actual start, one more upward gesture must leave the same frame.
    wait_screen(&f, &mut picker, &mut screen, |rows| {
        rows.first().is_some_and(|s| s == "ROW_01")
    });
    let top = screen.preview();
    picker.send(b"K");
    publish(4);
    assert_stable(&f, &mut picker, &mut screen, &top, 2);
    picker.send(b"\x1b[F");
    wait_screen(&f, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|s| s == "ROW_29")
    });
    picker.send(b"q");
    assert!(picker.finish().success());
}

#[test]
fn older_history_on_demand() {
    let mut fixture = setup("older-history");
    fixture.env("CONTENT", fixture.path("content"));
    fixture.write_json(
        "snapshot.json",
        &json!({
            "focused_pane_id": "p1", "panes": [{"pane_id": "p1"}],
            "agents": [agent("p1", "Agent", true)], "workspaces": [], "tabs": []
        }),
    );
    publish_rows(&fixture, 1, 95, "");
    let mut picker = fixture.spawn(&["picker"], 18, 100);
    let mut screen = Screen::new(18, 100);
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_95")
    });
    let initial = fixture
        .calls()
        .into_iter()
        .find(|call| call.starts_with(&["pane".into(), "read".into()]))
        .unwrap()[6]
        .parse::<usize>()
        .unwrap();
    assert!(
        initial < 95,
        "initial read must be a limited tail: {initial}"
    );

    let height = screen.preview().len();
    assert!(height > 1);
    let mut last = 95usize;
    let mut pages = 0;
    while last > height {
        picker.send(b"U");
        last = last.saturating_sub(height / 2).max(height);
        let expected: Vec<_> = (last + 1 - height..=last)
            .map(|row| format!("ROW_{row:02}"))
            .collect();
        let deadline = Instant::now() + WAIT;
        loop {
            screen.pump(&mut picker);
            if screen.preview() == expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected {expected:?}, got {:?}, calls {:?}",
                screen.preview(),
                fixture.calls()
            );
        }
        let settled = Instant::now() + Duration::from_millis(200);
        while Instant::now() < settled {
            screen.pump(&mut picker);
            assert_eq!(screen.preview(), expected, "viewport moved after paging");
        }
        pages += 1;
        if pages == 1 {
            publish_rows(&fixture, 1, 98, "");
            assert_stable(&fixture, &mut picker, &mut screen, &expected, 2);
        }
    }
    let top = screen.preview();
    assert_eq!(top.first().map(String::as_str), Some("ROW_01"));
    picker.send(b"U");
    assert_stable(&fixture, &mut picker, &mut screen, &top, 2);
    assert!(
        fixture.calls().iter().any(|call| {
            call.starts_with(&["pane".into(), "read".into()])
                && call[6].parse::<usize>().unwrap() > initial
        }),
        "older history must require a larger read"
    );

    picker.send(b"\x1b[F");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_98")
    });
    publish_rows(&fixture, 1, 99, "");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_99")
    });
    picker.send(b"q");
    assert!(picker.finish().success());
}

#[test]
fn codex_traverses_text_history_beyond_ansi_viewport() {
    let mut fixture = setup("codex-text-history");
    fixture.env("CONTENT", fixture.path("ansi"));
    fixture.env("TEXT_CONTENT", fixture.path("text"));
    let mut codex = agent("p1", "Codex", true);
    codex["agent"] = json!("codex");
    fixture.write_json(
        "snapshot.json",
        &json!({"focused_pane_id":"p1", "panes":[{"pane_id":"p1"}],
                "agents":[codex], "workspaces":[], "tabs":[]}),
    );
    let publish = |last| {
        let text: String = (1..=last).map(|n| format!("CX{n:03}\n")).collect();
        let ansi: String = (69..=last)
            .map(|n| format!("\x1b[32mCX{n:03}\x1b[0m\n"))
            .collect();
        fixture.write("text", &text);
        fixture.write(
            "ansi",
            &format!("Codex header CX001 (truncated)\n{ansi}Codex footer\n"),
        );
    };
    publish(120);
    for format in ["text", "ansi"] {
        let mut command = fixture.configure(std::process::Command::new(fixture.path("herdr")));
        command.args([
            "pane",
            "read",
            "p1",
            "--source",
            "recent-unwrapped",
            "--lines",
            "600",
            "--format",
            format,
        ]);
        let output = common::output(command, WAIT);
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        if format == "text" {
            assert_eq!(
                output,
                (1..=120).map(|n| format!("CX{n:03}\n")).collect::<String>()
            );
        } else {
            assert!(output.starts_with("Codex header CX001 (truncated)\n\x1b[32mCX069\x1b[0m\n"));
            assert!(output.ends_with("\x1b[32mCX120\x1b[0m\nCodex footer\n"));
            assert!(!output.contains("CX068"));
        }
    }
    let mut picker = fixture.spawn(&["picker"], 18, 100);
    let mut screen = Screen::new(18, 100);
    let markers = |screen: &Screen| -> Vec<usize> {
        screen
            .frame
            .iter()
            .filter_map(|row| {
                let text: String = row[43..97].iter().collect();
                let text = text.split_whitespace().next()?;
                (text.len() == 5 && text.starts_with("CX"))
                    .then(|| text[2..].parse().ok())
                    .flatten()
            })
            .collect()
    };
    let deadline = Instant::now() + WAIT;
    while !markers(&screen).contains(&120) {
        screen.pump(&mut picker);
        assert!(
            Instant::now() < deadline,
            "initial preview did not reach CX120: {:?}, calls {:?}",
            screen
                .frame
                .iter()
                .map(|row| row[43..97].iter().collect::<String>())
                .collect::<Vec<_>>(),
            fixture.calls()
        );
    }
    let mut previous = markers(&screen);
    let mut seen = std::collections::BTreeSet::new();
    loop {
        assert!(
            previous.windows(2).all(|w| w[1] == w[0] + 1),
            "nonconsecutive markers: {previous:?}"
        );
        seen.extend(previous.iter().copied());
        if previous[0] == 1 {
            break;
        }
        picker.send(b"U");
        let deadline = Instant::now() + WAIT;
        loop {
            screen.pump(&mut picker);
            let current = markers(&screen);
            // A half-page move can paint just its first rows before the rest arrive.
            if current.len() == 14 && current[0] < previous[0] {
                assert!(
                    current.iter().any(|n| previous.contains(n)),
                    "skipped between {previous:?} and {current:?}"
                );
                previous = current;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "stuck after {previous:?}, got {current:?}, calls {:?}",
                fixture.calls()
            );
        }
    }
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        (1..=120).collect::<Vec<_>>()
    );
    picker.send(b"\x1b[F");
    let deadline = Instant::now() + WAIT;
    while !markers(&screen).contains(&120) {
        screen.pump(&mut picker);
        assert!(Instant::now() < deadline, "failed to return to live tail");
    }
    publish(121);
    let deadline = Instant::now() + WAIT;
    while !markers(&screen).contains(&121) {
        screen.pump(&mut picker);
        assert!(
            Instant::now() < deadline,
            "live preview did not follow CX121"
        );
    }
    picker.send(b"q");
    assert!(picker.finish().success());
}

#[test]
fn origin_viewport() {
    keeps_preview_viewport("p1");
}

#[test]
fn attached_viewport() {
    keeps_preview_viewport("outside");
}

fn wrapped_rows(screen: &Screen) -> Vec<String> {
    let marker = regex::Regex::new(r"(?:ROW|END)_[0-9]{2}").unwrap();
    screen
        .frame
        .iter()
        .filter_map(|row| {
            let text = row[43..97].iter().collect::<String>();
            marker.find(&text).map(|value| value.as_str().to_owned())
        })
        .collect()
}

fn wait_wrapped(
    fixture: &Fixture,
    picker: &mut Pty,
    screen: &mut Screen,
    ready: impl Fn(&[String]) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        screen.pump(picker);
        if ready(&wrapped_rows(screen)) {
            return;
        }
    }
    panic!(
        "wrapped preview {:?}, Herdr events {:?}",
        wrapped_rows(screen),
        fixture.events()
    );
}

#[test]
fn wrapped_live_follow_resumes_at_exact_bottom() {
    let mut fixture = setup("wrapped-follow");
    fixture.env("CONTENT", fixture.path("content"));
    fixture.write_json(
        "snapshot.json",
        &json!({
            "focused_pane_id": "p1", "panes": [{"pane_id": "p1"}],
            "agents": [agent("p1", "Agent", true)], "workspaces": [], "tabs": []
        }),
    );
    let publish = |last: u8| {
        let content: String = (1..=last)
            .map(|row| format!("ROW_{row:02}{}END_{row:02}\n", "x".repeat(75)))
            .collect();
        fixture.write("content", &content);
    };
    publish(30);
    let mut picker = fixture.spawn(&["picker"], 18, 100);
    let mut screen = Screen::new(18, 100);
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_30"))
    });
    let bottom = wrapped_rows(&screen);
    assert!(bottom.len() >= 4, "{bottom:?}");
    assert!(bottom[bottom.len() - 2].contains("ROW_30"), "{bottom:?}");
    picker.send(b"K");
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_29"))
    });
    let frozen = wrapped_rows(&screen);
    assert_eq!(&frozen[2..], &bottom[..bottom.len() - 2]);
    publish(31);
    fixture.write_json(
        "snapshot.json",
        &json!({
            "focused_pane_id": "p1", "panes": [{"pane_id": "p1"}],
            "agents": [agent("p1", "Updated Task", true)], "workspaces": [], "tabs": []
        }),
    );
    let count = reads(&fixture, "p1");
    let deadline = Instant::now() + WAIT;
    while reads(&fixture, "p1") < count + 2 {
        assert!(Instant::now() < deadline);
        screen.pump(&mut picker);
        assert_eq!(wrapped_rows(&screen), frozen);
    }
    picker.send(b"\x1b[1;2B");
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("ROW_30"))
    });
    publish(32);
    let count = reads(&fixture, "p1");
    while reads(&fixture, "p1") < count + 2 {
        assert!(Instant::now() < deadline);
        screen.pump(&mut picker);
        assert!(wrapped_rows(&screen)
            .last()
            .is_some_and(|row| row.contains("ROW_30")));
    }
    picker.send(b"\x1b[1;2B");
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_32"))
    });
    picker.send(b"J");
    publish(33);
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_33"))
    });
    picker.send(b"K");
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_32"))
    });
    publish(34);
    picker.send(b"J");
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_34"))
    });
    picker.send(b"K");
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_33"))
    });
    publish(35);
    picker.send(b"D");
    wait_wrapped(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row.contains("END_35"))
    });
    picker.send(b"q");
    assert!(picker.finish().success());
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
    publish_rows(&fixture, 1, 80, "");
    let mut picker = fixture.spawn(&["picker"], 48, 100);
    let mut screen = Screen::new(48, 100);
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_80")
    });
    publish_rows(&fixture, 1, 90, "");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.len() > 30 && rows.last().is_some_and(|row| row == "ROW_90")
    });
    let bottom = screen.preview();
    picker.send(b"udK");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_88")
    });
    picker.send(b"J");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| rows == bottom);
    picker.send(b"D");
    screen.pump(&mut picker);
    publish_rows(&fixture, 1, 95, "");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.len() == bottom.len() && rows.last().is_some_and(|row| row == "ROW_95")
    });
    let bottom = screen.preview();
    picker.send(b"U");
    let half_page_tail = &bottom[bottom.len() - 1 - bottom.len() / 2];
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last() == Some(half_page_tail)
    });
    picker.send(b"D");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| rows == bottom);
    picker.send(b"\x1b]11;rgb:a8a8/a8a8/a8a8\x1b\\KKKK");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_87") && rows.len() == bottom.len()
    });
    let scrolled = screen.preview();
    assert_stable(&fixture, &mut picker, &mut screen, &scrolled, 2);
    let updated = json!({"focused_pane_id":"p1","panes":[{"pane_id":"p1"}],"agents":[agent("p1", "Updated Task", true)],"workspaces":[],"tabs":[]});
    fixture.write_json("snapshot.json", &updated);
    publish_rows(&fixture, 1, 99, "_EDIT");
    assert_stable(&fixture, &mut picker, &mut screen, &scrolled, 2);
    picker.send(b"JJJJ");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_99_EDIT")
    });
    picker.send(b"K");
    screen.pump(&mut picker);
    picker.send(b"K");
    let before_resize: Vec<_> = (96 - bottom.len()..=95)
        .map(|row| format!("ROW_{row:02}_EDIT"))
        .collect();
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows == before_resize
    });
    resize_and_wait(&fixture, &mut picker, &mut screen, 101, &before_resize);
    assert_stable(&fixture, &mut picker, &mut screen, &before_resize, 2);
    picker.send(b"\x1b[F");
    let deadline = Instant::now() + WAIT;
    loop {
        screen.pump(&mut picker);
        if screen
            .preview()
            .last()
            .is_some_and(|row| row == "ROW_99_EDIT")
            && screen
                .frame
                .iter()
                .any(|row| row.iter().collect::<String>().contains("Updated Task"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "preview {:?}, Herdr events {:?}",
            screen.preview(),
            fixture.events()
        );
    }
    picker.send(b"q");
    assert!(picker.finish().success());
}

#[test]
fn held_preview_survives_resize_at_top_and_near_bottom() {
    let mut fixture = setup("held-resize-edges");
    fixture.env("CONTENT", fixture.path("content"));
    fixture.write_json(
        "snapshot.json",
        &json!({
            "focused_pane_id": "p1", "panes": [{"pane_id": "p1"}],
            "agents": [agent("p1", "Agent", true)], "workspaces": [], "tabs": []
        }),
    );
    publish_rows(&fixture, 1, 80, "");
    let mut picker = fixture.spawn(&["picker"], 48, 100);
    let mut screen = Screen::new(48, 100);
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_80")
    });
    picker.send(b"\x1b[1;2A");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.last().is_some_and(|row| row == "ROW_79")
    });
    let near_bottom = screen.preview();
    resize_and_wait(&fixture, &mut picker, &mut screen, 101, &near_bottom);
    assert_stable(&fixture, &mut picker, &mut screen, &near_bottom, 2);
    picker.send(b"UUUUUU");
    wait_screen(&fixture, &mut picker, &mut screen, |rows| {
        rows.first().is_some_and(|row| row == "ROW_01")
    });
    let top = screen.preview();
    resize_and_wait(&fixture, &mut picker, &mut screen, 100, &top);
    assert_stable(&fixture, &mut picker, &mut screen, &top, 2);
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
        assert_eq!(
            fixture
                .events()
                .iter()
                .filter(|line| *line == "history-read p1")
                .count(),
            1
        );
        let log = fixture.events();
        let styled = log
            .iter()
            .position(|line| line == "styled-history-read p1")
            .unwrap();
        let attach = log
            .iter()
            .position(|line| line.starts_with("attach p1 "))
            .unwrap();
        assert!(styled < attach, "{log:?}");
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
    let history = log
        .iter()
        .position(|line| line == "history-read p1")
        .unwrap();
    assert!(history < attach && attach < read, "{log:?}");
    if scenario != "read-failure" {
        let styled = log
            .iter()
            .position(|line| line == "styled-history-read p1")
            .unwrap();
        assert!(history < styled && styled < attach, "{log:?}");
    }

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

#[test]
fn cross_tab_history_survives_attach_and_returns_to_selected_tail() {
    let mut fixture = setup("cross-tab-history");
    fixture.env("CROSS_TAB_CONTENT", fixture.path("cross-tab"));
    let mut b = agent("p2", "Agent B", false);
    b["tab_id"] = json!("t2");
    fixture.write_json(
        "snapshot.json",
        &json!({
            "focused_pane_id":"p1", "panes":[{"pane_id":"p1"},{"pane_id":"p2"}],
            "agents":[agent("p1", "Agent A", true), b],
            "workspaces":[{"workspace_id":"w1", "label":"Workspace"}],
            "tabs":[{"tab_id":"t1", "label":"Tab A"},{"tab_id":"t2", "label":"Tab B"}]
        }),
    );
    fixture.write("cross-tab/p1", "A_ONLY\n");
    let publish = |last| {
        fixture.write(
            "cross-tab/p2",
            &(1..=last).map(|n| format!("B{n:03}\n")).collect::<String>(),
        )
    };
    const LAST: usize = 2080;
    publish(LAST);
    let direct_read = |format| {
        let mut command = fixture.configure(std::process::Command::new(fixture.path("herdr")));
        command.args([
            "pane",
            "read",
            "p2",
            "--source",
            "recent-unwrapped",
            "--lines",
            "3000",
            "--format",
            format,
        ]);
        let output = common::output(command, WAIT);
        assert!(output.status.success());
        common::plain(&String::from_utf8(output.stdout).unwrap())
    };
    let full = (1..=LAST).map(|n| format!("B{n:03}\n")).collect::<String>();
    for format in ["text", "ansi"] {
        assert_eq!(direct_read(format), full);
    }

    let mut picker = fixture.spawn(&["picker"], 18, 100);
    let mut screen = Screen::new(18, 100);
    let rows = |screen: &Screen| -> Vec<String> {
        screen
            .frame
            .iter()
            .map(|row| row[43..97].iter().collect::<String>().trim().to_owned())
            .filter(|row| !row.is_empty())
            .collect()
    };
    let markers = |screen: &Screen| -> Vec<usize> {
        rows(screen)
            .iter()
            .filter_map(|row| {
                row.split_whitespace()
                    .next()?
                    .strip_prefix('B')?
                    .parse()
                    .ok()
            })
            .collect()
    };
    let deadline = Instant::now() + WAIT;
    while !rows(&screen).iter().any(|row| row.contains("A_ONLY")) {
        screen.pump(&mut picker);
        assert!(
            Instant::now() < deadline,
            "A not displayed: {:?}, {:?}",
            rows(&screen),
            fixture.events()
        );
    }
    picker.send(b"j");
    picker.wait_until(|| !attachments(&fixture, "p2").is_empty(), WAIT);
    for format in ["text", "ansi"] {
        assert_eq!(
            direct_read(format),
            (LAST - 36..=LAST)
                .map(|n| format!("B{n:03}\n"))
                .collect::<String>()
        );
    }
    let deadline = Instant::now() + WAIT;
    while !markers(&screen).contains(&LAST) {
        screen.pump(&mut picker);
        assert!(
            Instant::now() < deadline,
            "B tail missing: {:?}, {:?}",
            rows(&screen),
            fixture.events()
        );
    }
    let mut previous = markers(&screen);
    let mut seen = std::collections::BTreeSet::new();
    loop {
        assert!(
            !rows(&screen).iter().any(|row| row.contains("A_ONLY")),
            "A spliced into B: {:?}",
            rows(&screen)
        );
        assert!(
            previous.windows(2).all(|w| w[1] == w[0] + 1),
            "nonconsecutive B rows: {previous:?}"
        );
        seen.extend(previous.iter().copied());
        if previous[0] == 1 {
            break;
        }
        picker.send(if previous[0] <= 40 { b"K" } else { b"U" });
        let deadline = Instant::now() + WAIT;
        loop {
            screen.pump(&mut picker);
            let current = markers(&screen);
            if current.first().is_some_and(|first| *first < previous[0]) {
                assert!(
                    current.iter().any(|n| previous.contains(n)),
                    "skipped {previous:?} to {current:?}"
                );
                previous = current;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "stuck before B001: {previous:?}, now {:?}",
                rows(&screen)
            );
        }
    }
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        (1..=LAST).collect::<Vec<_>>()
    );
    let held = markers(&screen);
    publish(LAST + 1);
    let end = Instant::now() + Duration::from_millis(2300);
    while Instant::now() < end {
        screen.pump(&mut picker);
        assert_eq!(markers(&screen), held, "B history moved while held");
    }
    picker.send(b"\x1b[F");
    let deadline = Instant::now() + WAIT;
    while !markers(&screen).contains(&(LAST + 1)) {
        screen.pump(&mut picker);
        assert!(
            Instant::now() < deadline,
            "End did not show new row: {:?}",
            rows(&screen)
        );
    }
    publish(LAST + 2);
    let deadline = Instant::now() + WAIT;
    while !markers(&screen).contains(&(LAST + 2)) {
        screen.pump(&mut picker);
        assert!(
            Instant::now() < deadline,
            "live follow did not show next row: {:?}",
            rows(&screen)
        );
    }
    picker.send(b"\r");
    assert!(picker.finish().success());
    assert_eq!(fixture.focus().as_deref(), Some("p2"));
}
