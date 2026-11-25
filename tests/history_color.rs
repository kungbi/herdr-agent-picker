mod common;

use common::{Fixture, Pty, Screen};
use serde_json::json;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(8);

fn rows(screen: &Screen) -> Vec<usize> {
    let marker = regex::Regex::new(r"H[0-9]{3}").unwrap();
    screen
        .frame
        .iter()
        .filter_map(|row| {
            let text: String = row[43..97].iter().collect();
            marker.find(&text)?.as_str()[1..].parse().ok()
        })
        .collect()
}

fn wait_rows(picker: &mut Pty, screen: &mut Screen, ready: impl Fn(&[usize]) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        screen.pump(picker);
        let current = rows(screen);
        if ready(&current) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "preview rows: {current:?}, frame: {:?}",
            screen
                .frame
                .iter()
                .map(|r| r.iter().collect::<String>())
                .collect::<Vec<_>>()
        );
    }
}

fn assert_green(screen: &Screen, marker: &str) {
    for (y, row) in screen.frame.iter().enumerate() {
        let text: String = row[43..97].iter().collect();
        if let Some(x) = text.find(marker) {
            let colors = &screen.frame_colors[y][43 + x..43 + x + marker.len()];
            assert!(
                colors.iter().all(|color| matches!(
                    color.as_deref(),
                    Some([32]) | Some([38, 5, 2]) | Some([38, 2, 0, 128, 0])
                )),
                "{marker} painted with {colors:?}"
            );
            return;
        }
    }
    panic!("marker {marker} not painted: {:?}", rows(screen));
}

#[test]
fn styled_history_paging_and_viewport() {
    // Verify the color probe associates SGR with individual characters and resets.
    let mut probe = Screen::new(2, 100);
    probe.feed(b"\x1b[32mA\x1b[0mB\x1b[?7h");
    assert_eq!(probe.frame_colors[0][0], Some(vec![32]));
    assert_eq!(probe.frame_colors[0][1], None);

    for cross_tab in [false, true] {
        let mut fixture = Fixture::new(if cross_tab {
            "color-cross"
        } else {
            "color-origin"
        });
        fixture.env("FAKE_MODE", "resize");
        if cross_tab {
            fixture.env("CROSS_TAB_CONTENT", fixture.path("content"));
            fixture.write("content/p1", "ORIGIN\n");
            fixture.write(
                "content/p2",
                &(1..=120).map(|n| format!("H{n:03}\n")).collect::<String>(),
            );
        } else {
            fixture.env("CONTENT", fixture.path("ansi"));
            fixture.env("TEXT_CONTENT", fixture.path("text"));
            fixture.write(
                "ansi",
                &(1..=120)
                    .map(|n| format!("\x1b[32mH{n:03}\x1b[0m\n"))
                    .collect::<String>(),
            );
            fixture.write(
                "text",
                &(1..=120).map(|n| format!("H{n:03}\n")).collect::<String>(),
            );
        }
        fixture.write_json("snapshot.json", &json!({
            "focused_pane_id": "p1", "panes": [{"pane_id":"p1"},{"pane_id":"p2"}],
            "agents": [
                {"agent":"claude","pane_id":"p1","workspace_id":"w1","tab_id":"t1","agent_status":"working","title":"A","focused":true},
                {"agent":"claude","pane_id":"p2","workspace_id":"w1","tab_id":"t2","agent_status":"working","title":"B","focused":false}
            ], "workspaces":[], "tabs":[]
        }));
        fixture.env("DATA", fixture.path("snapshot.json"));
        let mut picker = fixture.spawn(&["picker"], 18, 100);
        let mut screen = Screen::new(18, 100);
        if cross_tab {
            picker.wait_until(|| fixture.events().iter().any(|e| e == "read p1"), WAIT);
            picker.send(b"j");
        }
        wait_rows(&mut picker, &mut screen, |r| r.last() == Some(&120));
        assert_green(&screen, "H120");
        let mut previous = rows(&screen);
        let mut checked_quick_navigation = false;
        while previous[0] > 1 {
            let expected = previous[0].saturating_sub(previous.len() / 2).max(1);
            picker.send(b"U");
            wait_rows(&mut picker, &mut screen, |r| {
                r.first() == Some(&expected)
                    && r.len() == previous.len()
                    && r.windows(2).all(|w| w[1] == w[0] + 1)
            });
            let current = rows(&screen);
            assert_eq!(current.len(), previous.len());
            assert!(current.windows(2).all(|w| w[1] == w[0] + 1), "{current:?}");
            assert_green(&screen, &format!("H{:03}", current[0]));
            previous = current;
            if !checked_quick_navigation && previous[0] <= 60 && previous[0] > 15 {
                picker.send(b"DU");
                let end = Instant::now() + Duration::from_millis(800);
                while Instant::now() < end {
                    screen.pump(&mut picker);
                }
                assert_eq!(rows(&screen), previous, "quick down/up changed position");
                checked_quick_navigation = true;
            }
        }
        assert!(checked_quick_navigation);
        let top = previous.clone();
        picker.send(b"U");
        let end = Instant::now() + Duration::from_millis(400);
        while Instant::now() < end {
            screen.pump(&mut picker);
            assert_eq!(rows(&screen), top);
        }
        picker.send(b"D");
        wait_rows(&mut picker, &mut screen, |r| {
            r.first().is_some_and(|n| *n > 1)
        });
        picker.send(b"U");
        wait_rows(&mut picker, &mut screen, |r| r == top);
        picker.send(b"\x1b[F");
        wait_rows(&mut picker, &mut screen, |r| r.last() == Some(&120));
        assert_green(&screen, "H120");
        picker.send(b"q");
        assert!(picker.finish().success());
    }
}
