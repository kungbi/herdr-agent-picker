use crate::util::*;
use regex::Regex;
use std::{
    collections::HashSet,
    env,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthChar;

// Only printable text, line breaks/tabs and SGR may reach fzf. In particular,
// OSC/DCS payloads, cursor movement, queries and alternate-screen controls must
// never reach the real terminal (even if pane read returns unexpected escapes).
fn sanitize(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 27 {
            let start = i;
            i += 1;
            match bytes.get(i) {
                Some(b'[') => {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                        if bytes[i - 1] == b'm'
                            && bytes[start + 2..i - 1]
                                .iter()
                                .all(|b| b.is_ascii_digit() || b";:".contains(b))
                        {
                            out.push_str(&text[start..i]);
                        }
                    }
                }
                Some(b']' | b'P' | b'X' | b'^' | b'_') => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != 7 && !bytes[i..].starts_with(b"\x1b\\") {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += if bytes[i] == 7 { 1 } else { 2 };
                    }
                }
                Some(_) => {
                    while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
                        i += 1;
                    }
                    // A malformed escape may precede UTF-8: do not split it.
                    if i < bytes.len() && bytes[i].is_ascii() {
                        i += 1;
                    }
                }
                None => (),
            }
        } else {
            let c = text[i..].chars().next().unwrap();
            if !c.is_control() || matches!(c, '\n' | '\t') {
                out.push(c);
            }
            i += c.len_utf8();
        }
    }
    out
}

pub fn clean(text: &str) -> String {
    let sanitized = sanitize(text);
    let text = sanitized.as_str();
    let styles = Regex::new(r"\x1b\[[0-9;:]*m").unwrap();
    let blank = Regex::new(r"^ +[│┃]$").unwrap();
    let edge = Regex::new(r"[│┃]((?:\x1b\[[0-9;:]*m)*)$").unwrap();
    let padding = Regex::new(r"(?:[ \t]|\x1b\[[0-9;:]*m)+$").unwrap();
    let rows: Vec<_> = text
        .split_inclusive('\n')
        .map(|line| {
            let line = line.trim_end_matches(['\r', '\n']);
            (line, styles.replace_all(line, "").into_owned())
        })
        .collect();
    let width = |s: &str| s.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>();
    let frames: HashSet<_> = rows
        .iter()
        .filter(|(_, plain)| blank.is_match(plain))
        .map(|(_, p)| width(p))
        .collect();
    let mut output = String::new();
    for (index, (line, plain)) in rows.iter().enumerate() {
        let line = if plain.ends_with(['│', '┃']) && frames.contains(&width(plain)) {
            edge.replace(line, "$1").into_owned()
        } else {
            line.to_string()
        };
        let line = padding.replace(&line, |caps: &regex::Captures| {
            styles
                .find_iter(&caps[0])
                .map(|m| m.as_str())
                .collect::<String>()
        });
        output.push_str(&line);
        if index + 1 < rows.len() || text.ends_with('\n') {
            output.push('\n');
        }
    }
    output
}
fn snapshot_limit() -> u16 {
    env::var("FZF_PREVIEW_LINES")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0)
        .saturating_mul(2)
        .max(30)
}
pub fn read_snapshot_lines(pane: &str, lines: u16) -> Result<String> {
    command_output(
        Command::new(herdr()).args([
            "pane",
            "read",
            pane,
            "--source",
            "recent-unwrapped",
            "--lines",
            &lines.to_string(),
            "--format",
            "ansi",
        ]),
        Duration::from_secs(3),
    )
    .map(|text| clean(&text))
}
fn read_snapshot(pane: &str) -> Result<String> {
    read_snapshot_lines(pane, snapshot_limit())
}
pub fn snapshot(pane: &str) -> Result<bool> {
    if pane.is_empty() || pane == "@section" {
        return Ok(false);
    }
    match read_snapshot(pane) {
        Ok(text) => {
            print!("{text}");
            Ok(true)
        }
        Err(_) => {
            println!("Preview unavailable");
            Ok(false)
        }
    }
}

// pane read is already sanitized: SGR is the only remaining escape sequence.
pub fn visual_rows(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut row = String::new();
        let mut cells = 0;
        let mut sgr = false;
        for ch in line.chars() {
            if ch == '\x1b' || sgr {
                row.push(ch);
                sgr = ch != 'm';
                continue;
            }
            let n = if ch == '\t' {
                8 - cells % 8
            } else {
                ch.width().unwrap_or(0)
            };
            if ch == '\t' {
                for _ in 0..n {
                    if cells == width {
                        rows.push(std::mem::take(&mut row));
                        cells = 0;
                    }
                    row.push(' ');
                    cells += 1;
                }
            } else {
                if n > 0 && cells > 0 && cells + n > width {
                    rows.push(std::mem::take(&mut row));
                    cells = 0;
                }
                row.push(ch);
                cells += n;
            }
        }
        rows.push(row);
    }
    rows
}

#[derive(Default)]
struct SnapshotStream {
    previous: Vec<String>,
    buffer: Vec<String>,
    restored: bool,
    height: usize,
    width: usize,
    emitted: usize,
}
impl SnapshotStream {
    fn write(&mut self, text: &str, out: &mut impl Write) -> Result<()> {
        let next = visual_rows(text, self.width.max(1));
        if next == self.previous {
            return Ok(());
        }
        // fzf 0.73's CSI 2 J resets both the offset and paused follow state.
        // Stream overlapping snapshots as terminal history instead: fzf keeps
        // the viewport and resumes following only when scrolled to the bottom.
        let overlap = (1..=self.previous.len().min(next.len()))
            .rev()
            .find(|&n| {
                self.previous[self.previous.len() - n..] == next[..n]
                    && next[..n].iter().any(|line| !line.trim().is_empty())
            })
            .unwrap_or(0);
        if overlap == 0 && self.emitted > 0 {
            // A working agent may repaint old rows. Replace the live buffer, not its history twice.
            out.write_all(b"\x1b[2J")?;
            self.emitted = 0;
            self.buffer.clear();
        }
        self.buffer.extend_from_slice(&next[overlap..]);
        self.emitted += next.len() - overlap + usize::from(next.is_empty());
        for line in &next[overlap..] {
            writeln!(out, "{line}")?;
        }
        // A newline makes even an empty replacement visible to fzf's reader.
        if next.is_empty() {
            writeln!(out)?;
        }
        out.flush()?;
        self.previous = next;
        Ok(())
    }
    fn refresh(&mut self, pane: &str, history: &Path) -> Result<bool> {
        match read_snapshot(pane) {
            Ok(text) => {
                // Serialize publication with the first scroll key, never with the slow pane read.
                let _guard = lock(&history.with_extension("viewport-lock"), false)?;
                if self.emitted == 0 || !history.exists() {
                    if self.restored && !history.exists() {
                        std::io::stdout().write_all(b"\x1b[2J")?;
                        self.previous.clear();
                        self.buffer.clear();
                        self.emitted = 0;
                        self.restored = false;
                    }
                    let before = self.emitted;
                    let previous = self.previous.clone();
                    self.write(&text, &mut std::io::stdout().lock())?;
                    if self.emitted != before || self.previous != previous {
                        atomic_write(
                            &history.with_extension("extent"),
                            format!("{} {}", self.emitted, self.height).as_bytes(),
                        )?;
                        atomic_write(
                            &history.with_extension("window"),
                            serde_json::to_string(&serde_json::json!({
                                "pane": pane, "width": self.width, "limit": snapshot_limit(),
                                "exhausted": false, "rows": self.buffer,
                            }))?
                            .as_bytes(),
                        )?;
                    }
                }
                Ok(true)
            }
            Err(_) => {
                self.write("Preview unavailable", &mut std::io::stdout().lock())?;
                Ok(false)
            }
        }
    }
}
fn cloexec(fd: RawFd, enabled: bool) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0
        || unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFD,
                if enabled {
                    flags | libc::FD_CLOEXEC
                } else {
                    flags & !libc::FD_CLOEXEC
                },
            )
        } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn poll(fd: RawFd, ms: i32) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}
fn finish(child: &mut Child) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}
pub fn guard(terminal: &str, control: RawFd, lock_fd: RawFd) -> Result<()> {
    signals();
    cloexec(control, true)?;
    cloexec(lock_fd, true)?;
    // Own these inherited descriptors until the attachment is fully reaped. A
    // killed fzf preview closes the writer; the guard survives in its own session.
    let _lock = unsafe { File::from_raw_fd(lock_fd) };
    let mut pipe = unsafe { File::from_raw_fd(control) };
    let mut child = Command::new(herdr())
        .args(["terminal", "attach", terminal])
        .spawn()?;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if stopped() {
            break;
        }
        if poll(control, 100) {
            let mut byte = [0];
            if pipe.read(&mut byte)? == 0 {
                break;
            }
        }
    }
    if child.try_wait()?.is_none() {
        finish(&mut child);
    }
    Ok(())
}
struct Attachment {
    child: Child,
    control: Option<File>,
    master: File,
}
impl Drop for Attachment {
    fn drop(&mut self) {
        self.control.take();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        finish(&mut self.child);
    }
}
fn attach(terminal: &str, lock: &File, columns: u16, lines: u16) -> Result<Attachment> {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: lines,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(size),
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    cloexec(master.as_raw_fd(), true)?;
    cloexec(slave.as_raw_fd(), true)?;
    let mut pipe = [-1; 2];
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let read = unsafe { File::from_raw_fd(pipe[0]) };
    let write = unsafe { File::from_raw_fd(pipe[1]) };
    cloexec(write.as_raw_fd(), true)?;
    let mut command = Command::new(executable()?);
    command
        .args([
            "--attach-guard",
            terminal,
            &read.as_raw_fd().to_string(),
            &lock.as_raw_fd().to_string(),
        ])
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    let lock_fd = lock.as_raw_fd();
    let control_fd = read.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(lock_fd, libc::F_SETFD, 0) < 0
                || libc::fcntl(control_fd, libc::F_SETFD, 0) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let child = command.spawn()?;
    drop(read);
    Ok(Attachment {
        child,
        control: Some(write),
        master,
    })
}
fn live_inner(pane: &str, path: &Path, origin: &str) -> Result<()> {
    let size = |key: &str| -> Result<u16> {
        let n = env::var(key)?.parse::<u16>()?;
        if n == 0 {
            return Err("Invalid preview grid".into());
        }
        Ok(n)
    };
    let (columns, lines) = (size("FZF_PREVIEW_COLUMNS")?, size("FZF_PREVIEW_LINES")?);
    let lock = lock(path, false)?.ok_or("Preview lock unavailable")?;
    if stopped() {
        return Ok(());
    }
    let mut stream = SnapshotStream {
        width: usize::from(columns),
        height: usize::from(lines),
        ..Default::default()
    };
    let history = path.with_extension("history");
    if history.exists() {
        if let Ok(data) = std::fs::read(history.with_extension("window")) {
            if let Ok(window) = serde_json::from_slice::<serde_json::Value>(&data) {
                if window["pane"].as_str() == Some(pane) {
                    if let Ok(rows) = serde_json::from_value::<Vec<String>>(window["rows"].clone())
                    {
                        let mut out = std::io::stdout().lock();
                        for row in &rows {
                            writeln!(out, "{row}")?;
                        }
                        out.flush()?;
                        stream.emitted = rows.len();
                        stream.previous = rows.clone();
                        stream.buffer = rows;
                        stream.restored = true;
                    }
                }
            }
        }
    }
    let mut input = crate::interaction::Input::new(path)?;
    // Unknown origin is deliberately read-only too: guessing could resize the
    // visible pane. Keep the lock so a previous attachment is reaped first.
    if origin.is_empty() || pane == origin {
        while !stopped() {
            if !stream.refresh(pane, &history)? {
                return Ok(());
            }
            let end = Instant::now() + Duration::from_secs(1);
            while Instant::now() < end && !stopped() {
                input.update(pane, None, None)?;
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        return Ok(());
    }
    let info = request(&["pane", "get", pane])?;
    let terminal = info["pane"]["terminal_id"]
        .as_str()
        .ok_or("Terminal unavailable")?;
    let mut attached = attach(terminal, &lock, columns, lines)?;
    let mut data = Vec::new();
    let end = Instant::now() + Duration::from_secs(3);
    let mut bytes = [0; 65536];
    while !data.windows(4).any(|w| w == b"\x1b[2J") {
        if stopped() {
            return Ok(());
        }
        if attached.child.try_wait()?.is_some() || Instant::now() > end {
            return Err("Terminal attachment unavailable".into());
        }
        if poll(attached.master.as_raw_fd(), 100) {
            let n = attached.master.read(&mut bytes)?;
            if n == 0 {
                return Err("Terminal attachment ended".into());
            }
            data.extend_from_slice(&bytes[..n]);
            if data.len() > 65536 {
                data.drain(..data.len() - 65536);
            }
        }
    }
    let mut refresh = Instant::now();
    while !stopped() {
        if attached.child.try_wait()?.is_some() {
            return Err("Terminal attachment ended".into());
        }
        input.update(pane, Some(terminal), Some(&mut attached.master))?;
        if Instant::now() >= refresh {
            if !stream.refresh(pane, &history)? {
                return Err("Preview read failed; input stopped".into());
            }
            refresh = Instant::now()
                + if input.active() {
                    Duration::from_millis(100)
                } else {
                    Duration::from_secs(1)
                };
        }
        if poll(attached.master.as_raw_fd(), 20) {
            let n = attached.master.read(&mut bytes)?;
            if n == 0 {
                return Err("Terminal attachment ended".into());
            }
            // Drain the attachment, never replay terminal control sequences to
            // the picker. Herdr's sanitized snapshots go through fzf's grid.
            if input.active() {
                refresh = Instant::now();
            }
        }
    }
    Ok(())
}
pub fn live(pane: &str, path: &Path, origin: &str) {
    if pane.is_empty() || pane == "@section" {
        return;
    }
    signals();
    if let Err(error) = live_inner(pane, path, origin) {
        if stopped() {
            return;
        }
        println!("Preview unavailable: {}", label(&error.to_string()));
        let _ = std::io::stdout().flush();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_escape_allowlist_confines_output() {
        assert_eq!(
            sanitize("한글\x1b[2J\x1b[1;1Htext\x1b[?1049h\x1b]52;c;secret\x07\x1bPpayload\x1b\\\x1b^payload\x1b\\\x1b[38;2;1;2;3m色\x1b[0m\n\t\r\x00\x08\u{009b}"),
            "한글text\x1b[38;2;1;2;3m色\x1b[0m\n\t"
        );
        for tail in ["\x1b", "\x1b[1;", "\x1b]unfinished", "\x1bPunfinished"] {
            assert_eq!(sanitize(&format!("safe{tail}")), "safe");
        }
        assert_eq!(sanitize("\x1b한글"), "한글");
    }
    #[test]
    fn live_snapshot_deltas() {
        let mut stream = SnapshotStream {
            height: 3,
            width: 40,
            ..Default::default()
        };
        let mut out = Vec::new();
        stream.write("\x1b[32mA\x1b[0m\nB\n", &mut out).unwrap();
        assert_eq!(out, b"\x1b[32mA\x1b[0m\nB\n");
        out.clear();
        for _ in 0..3 {
            stream.write("\x1b[32mA\x1b[0m\nB\n", &mut out).unwrap();
        }
        assert!(out.is_empty());
        stream.write("\x1b[32mA\x1b[0m\nB\nC", &mut out).unwrap();
        assert_eq!(out, b"C\n");
        out.clear();
        stream.write("B\nC\nD\n", &mut out).unwrap();
        assert_eq!(out, b"D\n");
        out.clear();
        stream.write("Edited\n", &mut out).unwrap();
        assert_eq!(out, b"\x1b[2JEdited\n");
        out.clear();
        stream.write("", &mut out).unwrap();
        assert_eq!(out, b"\x1b[2J\n");
    }
    #[test]
    fn padding_and_styles() {
        let text="    한글 e\u{301} │ \x1b[0m┃\x1b[31m\n              ┃\n│ table │\n    code \x1b[0m  \x1b[31m \x1b[0m\n";
        let cleaned = clean(text);
        assert_eq!(clean("A\n\nB\n"), "A\n\nB\n");
        assert_eq!(clean("│ table │\nliteral ┃\n"), "│ table │\nliteral ┃\n");
        let re = Regex::new(r"\x1b\[[0-9;:]*m").unwrap();
        assert_eq!(
            re.find_iter(text).map(|m| m.as_str()).collect::<Vec<_>>(),
            re.find_iter(&cleaned)
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
        );
        assert!(cleaned.contains("    code\x1b[0m\x1b[31m\x1b[0m\n"));
    }
}
