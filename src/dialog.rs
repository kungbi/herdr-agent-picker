use crate::util::*;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
};
use unicode_width::UnicodeWidthChar;
const TEXT: &str = "\x1b[0;39;49m";
const BORDER: &str = "\x1b[0;38;5;59;49m";
const SELECTED: &str = "\x1b[0;39;48;5;236m";
pub struct Form {
    pub values: [i32; 5],
    pub index: usize,
    pub draft: Option<String>,
}
impl Form {
    pub fn new(initial: Option<f64>) -> Self {
        let t = local(initial.unwrap_or_else(|| now() + 1800.));
        Self {
            values: [
                t.tm_year + 1900,
                t.tm_mon + 1,
                t.tm_mday,
                t.tm_hour,
                t.tm_min,
            ],
            index: 2,
            draft: None,
        }
    }
    pub fn commit(&mut self) -> Result<()> {
        if let Some(text) = self.draft.as_ref().filter(|s| !s.is_empty()) {
            let v = text.parse::<i32>()?;
            let (low, high) = [(1, 9999), (1, 12), (1, 31), (0, 23), (0, 59)][self.index];
            if v < low || v > high {
                return Err(format!(
                    "{}: use {low}-{high}.",
                    ["Year", "Month", "Day", "Hour", "Minute"][self.index]
                )
                .into());
            }
            self.values[self.index] = v;
        }
        self.draft = None;
        Ok(())
    }
    pub fn move_field(&mut self, step: i32) -> Result<()> {
        self.commit()?;
        self.index = (self.index as i32 + step).rem_euclid(5) as usize;
        Ok(())
    }
    pub fn digit(&mut self, c: char) {
        let width = if self.index == 0 { 4 } else { 2 };
        let s = self.draft.get_or_insert_with(String::new);
        if s.len() >= width {
            s.clear();
        }
        s.push(c);
    }
    pub fn backspace(&mut self) {
        self.draft
            .get_or_insert_with(|| self.values[self.index].to_string())
            .pop();
    }
    pub fn parts(&self) -> Vec<String> {
        self.values
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let s = format!("{:0width$}", n, width = if i == 0 { 4 } else { 2 });
                if i == self.index {
                    format!("[{}]", self.draft.as_ref().unwrap_or(&s))
                } else {
                    s
                }
            })
            .collect()
    }
    pub fn adjust(&mut self, step: i32) -> Result<()> {
        self.commit()?;
        let mut v = self.values;
        if self.index < 2 {
            if self.index == 0 {
                v[0] += step
            } else {
                let offset = v[0] * 12 + v[1] - 1 + step;
                v[0] = offset.div_euclid(12);
                v[1] = offset.rem_euclid(12) + 1;
            }
            let max = days(v[0], v[1]);
            v[2] = v[2].min(max);
        } else {
            if !(1..=12).contains(&v[1])
                || !(1..=days(v[0], v[1])).contains(&v[2])
                || !(0..=23).contains(&v[3])
                || !(0..=59).contains(&v[4])
            {
                return Err("Invalid calendar date.".into());
            }
            v[self.index] += step;
        }
        if !(1..=9999).contains(&v[0]) {
            return Err("Year: use 1-9999.".into());
        }
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = v[0] - 1900;
        tm.tm_mon = v[1] - 1;
        tm.tm_mday = v[2];
        tm.tm_hour = v[3];
        tm.tm_min = v[4];
        let stamp = unsafe { libc::timegm(&mut tm) };
        let mut t: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::gmtime_r(&stamp, &mut t);
        }
        let values = [
            t.tm_year + 1900,
            t.tm_mon + 1,
            t.tm_mday,
            t.tm_hour,
            t.tm_min,
        ];
        if !(1..=9999).contains(&values[0]) {
            return Err("Year: use 1-9999.".into());
        }
        self.values = values;
        Ok(())
    }
    pub fn due(&mut self, now: f64) -> Result<f64> {
        self.commit()?;
        let due = calendar(self.values, true)?;
        if due <= now {
            return Err("Choose a future date and time.".into());
        }
        Ok(due)
    }
}
fn days(y: i32, m: i32) -> i32 {
    match m {
        2 => {
            if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}
fn fitted(text: &str, width: usize) -> String {
    let mut s = String::new();
    let mut used = 0;
    for c in text.chars().filter(|c| !c.is_control()) {
        let n = c.width().unwrap_or(0);
        if used + n > width {
            break;
        }
        s.push(c);
        used += n;
    }
    s.push_str(&" ".repeat(width - used));
    s
}
pub struct Dialog {
    tty: File,
    original: libc::termios,
    bracketed: bool,
}
impl Drop for Dialog {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.tty.as_raw_fd(), libc::TCSANOW, &self.original);
        }
        let _ = self.tty.write_all(b"\x1b[0m\x1b[?25h");
        if self.bracketed {
            let _ = self.tty.write_all(b"\x1b[?2004l");
        }
    }
}
impl Dialog {
    pub fn new() -> Result<Self> {
        let path = std::env::var_os("HERDR_AGENT_PICKER_TTY_PATH")
            .map(std::path::PathBuf::from)
            .or_else(|| tty_path(2))
            .unwrap_or_else(|| std::path::PathBuf::from("/dev/tty"));
        let tty = OpenOptions::new().read(true).write(true).open(path)?;
        let mut original = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(tty.as_raw_fd(), &mut original) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut d = Self {
            tty,
            original,
            bracketed: false,
        };
        d.raw()?;
        d.tty.write_all(b"\x1b[?25l")?;
        Ok(d)
    }
    fn raw(&self) -> Result<()> {
        let mut raw = self.original;
        unsafe {
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(self.tty.as_raw_fd(), libc::TCSANOW, &raw) < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    }
    fn ready(&self, ms: i32) -> bool {
        let mut p = libc::pollfd {
            fd: self.tty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut p, 1, ms) > 0 }
    }
    fn size(&self) -> (usize, usize) {
        let mut s: libc::winsize = unsafe { std::mem::zeroed() };
        unsafe { libc::ioctl(self.tty.as_raw_fd(), libc::TIOCGWINSZ, &mut s) };
        (s.ws_col as usize, s.ws_row as usize)
    }
    fn composer_line(&mut self, recipient: &str, text: &str, note: &str) -> Result<()> {
        let (columns, rows) = self.size();
        let width = columns.saturating_sub(1);
        let prefix = format!("To {} > ", label(recipient));
        let prefix = fitted(&prefix, width / 2);
        let available = width.saturating_sub(width / 2);
        let mut tail = String::new();
        let mut used = 0;
        for c in text.chars().rev() {
            let n = c.width().unwrap_or(0);
            if used + n >= available {
                break;
            }
            tail.insert(0, c);
            used += n;
        }
        if !note.is_empty() || text.is_empty() {
            tail = fitted(
                if note.is_empty() {
                    "Enter: send / Esc: cancel"
                } else {
                    note
                },
                available,
            );
        }
        write!(
            self.tty,
            "\x1b[{};1H{SELECTED}\x1b[2K{prefix}{tail}\x1b[0m\x1b[?25h",
            rows.max(1)
        )?;
        self.tty.flush()?;
        Ok(())
    }
    pub fn compose(&mut self, recipient: &str) -> Result<Option<String>> {
        self.bracketed = true;
        self.tty.write_all(b"\x1b[?2004h")?;
        let mut input = ComposerInput::default();
        loop {
            self.composer_line(recipient, &input.text, &input.note)?;
            self.raw()?;
            if !self.ready(if input.escape.is_empty() { 100 } else { 40 }) {
                if input.escape == b"\x1b" {
                    return Ok(None);
                }
                // Keep incomplete CSI/paste markers inert across delayed reads.
                continue;
            }
            let mut byte = [0];
            if self.tty.read(&mut byte)? == 0 {
                return Ok(None);
            }
            match input.feed(byte[0]) {
                ComposerEvent::Submit => {
                    // Unbracketed multiline paste can contain apparent Enter keys.
                    // Reject a queued input burst rather than submit its first line
                    // and leak the remaining keys back into the picker.
                    if self.ready(0) {
                        while self.ready(20) {
                            if self.tty.read(&mut byte)? == 0 {
                                return Ok(None);
                            }
                        }
                        input = ComposerInput::default();
                        input.note = "Queued input after Enter rejected; paste one line".into();
                    } else {
                        return Ok(Some(input.text));
                    }
                }
                ComposerEvent::Cancel => return Ok(None),
                ComposerEvent::Continue => (),
            }
        }
    }
    pub fn composer_error(&mut self, error: &str) -> Result<()> {
        // Stay on the bottom row; fzf's list and preview remain visible.
        self.bracketed = true;
        self.tty.write_all(b"\x1b[?2004h")?;
        let mut input = ComposerInput::default();
        loop {
            let (columns, rows) = self.size();
            write!(
                self.tty,
                "\x1b[{};1H{SELECTED}\x1b[2K{}\x1b[0m",
                rows.max(1),
                fitted(
                    &format!("[Enter/Esc: back] Error: {}", label(error)),
                    columns.saturating_sub(1)
                )
            )?;
            self.raw()?;
            if !self.ready(100) {
                if input.escape == b"\x1b" {
                    return Ok(());
                }
                continue;
            }
            let mut byte = [0];
            if self.tty.read(&mut byte)? == 0 {
                return Ok(());
            }
            let enter = matches!(byte[0], 10 | 13)
                && input.escape.is_empty()
                && input.paste.is_none()
                && input.utf8.is_empty();
            if enter {
                // Consume queued pasted keys before returning control to fzf.
                while self.ready(20) {
                    if self.tty.read(&mut byte)? == 0 {
                        break;
                    }
                }
                return Ok(());
            }
            if matches!(input.feed(byte[0]), ComposerEvent::Cancel) {
                return Ok(());
            }
            input.text.clear();
        }
    }
    fn key(&mut self) -> Result<Option<String>> {
        // fzf can restore cooked input after execute-silent has been running 1s.
        self.raw()?;
        if !self.ready(100) {
            return Ok(None);
        }
        let mut b = [0];
        if self.tty.read(&mut b)? == 0 {
            return Ok(Some("escape".into()));
        }
        let key = match b[0] {
            27 => {
                let mut seq = vec![];
                while self.ready(40) {
                    if self.tty.read(&mut b)? == 0 {
                        break;
                    }
                    seq.push(b[0]);
                    if seq.len() >= 2 && (0x40..=0x7e).contains(&b[0]) {
                        break;
                    }
                    if seq.len() > 16 {
                        break;
                    }
                }
                match seq.as_slice() {
                    [] => "escape",
                    b"[A" | b"OA" => "up",
                    b"[B" | b"OB" => "down",
                    b"[C" | b"OC" => "right",
                    b"[D" | b"OD" => "left",
                    b"[Z" => "shift-tab",
                    b"[H" | b"OH" | b"[1~" => "home",
                    b"[F" | b"OF" | b"[4~" => "end",
                    _ => return Ok(None),
                }
            }
            3 | 4 => "escape",
            10 | 13 => "enter",
            8 | 127 => "backspace",
            9 => "tab",
            c if c.is_ascii() => return Ok(Some((c as char).to_string())),
            _ => return Ok(None),
        };
        Ok(Some(key.into()))
    }
    fn draw(&mut self, title: &str, lines: &[String], selected: Option<usize>) -> Result<()> {
        let (columns, rows) = self.size();
        let width = 66.min(columns.saturating_sub(4).max(4));
        let height = 14.min(rows.saturating_sub(2).max(3));
        let (top, left) = (
            (rows.saturating_sub(height)) / 2 + 1,
            (columns.saturating_sub(width)) / 2 + 1,
        );
        let inner = width - 2;
        let title = fitted(&format!(" {title} "), inner.saturating_sub(2))
            .trim_end()
            .to_owned();
        let border = format!("─{title}");
        let mut panel = vec![format!(
            "╭{}{}╮",
            border,
            "─".repeat(inner.saturating_sub(border.chars().count()))
        )];
        for i in 0..height - 2 {
            panel.push(format!(
                "│{}│",
                fitted(lines.get(i).map(String::as_str).unwrap_or(""), inner)
            ));
        }
        panel.push(format!("╰{}╯", "─".repeat(inner)));
        let mut output = format!("{TEXT}\x1b[?25l");
        for (i, line) in panel.iter().enumerate() {
            let styled = if i == 0 || i == panel.len() - 1 {
                format!("{BORDER}{line}")
            } else {
                let chars: Vec<_> = line.chars().collect();
                format!(
                    "{BORDER}│{}{}{BORDER}│",
                    if selected == Some(i - 1) {
                        SELECTED
                    } else {
                        TEXT
                    },
                    chars[1..chars.len() - 1].iter().collect::<String>()
                )
            };
            output.push_str(&format!("\x1b[{};{left}H{styled}\x1b[0m", top + i));
        }
        self.tty.write_all(output.as_bytes())?;
        Ok(())
    }
    pub fn choose(&mut self, rows: &[String], header: &str, title: &str) -> Result<Option<String>> {
        if rows.is_empty() {
            return Ok(None);
        }
        let mut selected: usize = 0;
        loop {
            let headings: Vec<String> = header.lines().take(2).map(String::from).collect();
            let capacity = 9.min(self.size().1.saturating_sub(headings.len() + 7).max(1));
            let start = (selected + 1).saturating_sub(capacity);
            let mut lines = headings.clone();
            lines.push(String::new());
            for (i, row) in rows.iter().enumerate().skip(start).take(capacity) {
                lines.push(format!(
                    "{}{}",
                    if i == selected { "> " } else { "  " },
                    row.split_once('\t').map(|x| x.1).unwrap_or(row)
                ));
            }
            self.draw(title, &lines, Some(headings.len() + 1 + selected - start))?;
            if let Some(key) = self.key()? {
                match key.as_str() {
                    "escape" => return Ok(None),
                    "enter" => return Ok(Some(rows[selected].split('\t').next().unwrap().into())),
                    "j" | "down" => selected = (selected + 1).min(rows.len() - 1),
                    "k" | "up" => selected = selected.saturating_sub(1),
                    s => {
                        if let Ok(n) = s.parse::<usize>() {
                            if (1..=9).contains(&n) && n <= rows.len() {
                                return Ok(Some(rows[n - 1].split('\t').next().unwrap().into()));
                            }
                        }
                    }
                }
            }
        }
    }
    pub fn date_time(&mut self, initial: Option<f64>) -> Result<Option<f64>> {
        let mut form = Form::new(initial);
        let mut error = String::new();
        loop {
            let p = form.parts();
            let lines = [
                "Date and time (local)".into(),
                "←/→ / Tab: field   ↑/↓ / j/k: change".into(),
                "".into(),
                format!("Date:  {} - {} - {}", p[0], p[1], p[2]),
                format!("Time:  {} : {}", p[3], p[4]),
                "".into(),
                "Digits: replace selected field".into(),
                "Enter: save   Esc: cancel".into(),
                "".into(),
                error.clone(),
            ];
            self.draw(
                "Custom time",
                &lines,
                Some(if form.index < 3 { 3 } else { 4 }),
            )?;
            let Some(key) = self.key()? else { continue };
            if key == "escape" {
                return Ok(None);
            }
            let mut action = || -> Result<Option<f64>> {
                match key.as_str() {
                    "enter" => return Ok(Some(form.due(now())?)),
                    "left" | "h" | "shift-tab" => form.move_field(-1)?,
                    "right" | "l" | "tab" => form.move_field(1)?,
                    "home" | "end" => {
                        form.commit()?;
                        form.index = if key == "home" { 0 } else { 4 };
                    }
                    "up" | "k" | "down" | "j" => {
                        form.adjust(if key == "up" || key == "k" { 1 } else { -1 })?
                    }
                    "backspace" => form.backspace(),
                    s if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => {
                        form.digit(s.chars().next().unwrap())
                    }
                    _ => (),
                }
                Ok(None)
            };
            match action() {
                Ok(Some(due)) => return Ok(Some(due)),
                Ok(None) => error.clear(),
                Err(e) => error = e.to_string(),
            }
        }
    }
    pub fn message(&mut self, text: &str) -> Result<()> {
        loop {
            let mut lines: Vec<String> = text.lines().map(String::from).collect();
            lines.extend(["".into(), "Enter / Esc: back".into()]);
            self.draw("Alarm", &lines, None)?;
            if matches!(self.key()?.as_deref(), Some("enter" | "escape")) {
                return Ok(());
            }
        }
    }
}
const COMPOSER_LIMIT: usize = 16 * 1024;
#[derive(Default)]
struct ComposerInput {
    text: String,
    utf8: Vec<u8>,
    escape: Vec<u8>,
    paste: Option<Vec<u8>>,
    paste_end: usize,
    paste_overflow: bool,
    note: String,
}
enum ComposerEvent {
    Continue,
    Submit,
    Cancel,
}
fn safe_text(s: &str) -> bool {
    static UNSAFE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    !UNSAFE
        .get_or_init(|| regex::Regex::new(r"[\p{C}\p{Zl}\p{Zp}]").unwrap())
        .is_match(s)
}
impl ComposerInput {
    fn append(&mut self, s: &str) {
        if !safe_text(s) {
            self.note = "Control/multiline input rejected".into();
        } else if self.text.len() + s.len() > COMPOSER_LIMIT {
            self.note = "Input exceeds 16 KiB".into();
        } else {
            self.text.push_str(s);
            self.note.clear();
        }
    }
    fn feed(&mut self, byte: u8) -> ComposerEvent {
        if let Some(paste) = self.paste.as_mut() {
            const END: &[u8] = b"\x1b[201~";
            if paste.len() < COMPOSER_LIMIT + END.len() {
                paste.push(byte);
            } else {
                self.paste_overflow = true;
            }
            self.paste_end = if byte == END[self.paste_end] {
                self.paste_end + 1
            } else {
                usize::from(byte == END[0])
            };
            if self.paste_end == END.len() {
                let mut bytes = self.paste.take().unwrap();
                if self.paste_overflow {
                    self.note = "Paste exceeds 16 KiB; rejected".into();
                } else {
                    bytes.truncate(bytes.len() - END.len());
                    match String::from_utf8(bytes) {
                        Ok(s) => self.append(&s),
                        Err(_) => self.note = "Invalid UTF-8 paste rejected".into(),
                    }
                }
                self.paste_end = 0;
                self.paste_overflow = false;
            }
            return ComposerEvent::Continue;
        }
        if !self.escape.is_empty() {
            if byte == 27 {
                self.escape = vec![27];
                return ComposerEvent::Continue;
            }
            if matches!(byte, 3 | 4) {
                return ComposerEvent::Cancel;
            }
            self.escape.push(byte);
            if self.escape == b"\x1b[200~" {
                self.paste = Some(vec![]);
                self.escape.clear();
            } else if (self.escape.len() >= 3 && (0x40..=0x7e).contains(&byte))
                || (self.escape.len() == 2 && !b"[O".contains(&byte))
            {
                self.escape.clear();
            } else if self.escape.len() > 32 {
                self.escape.truncate(32);
            }
            return ComposerEvent::Continue;
        }
        if !self.utf8.is_empty() || byte >= 128 {
            self.utf8.push(byte);
            match std::str::from_utf8(&self.utf8) {
                Ok(s) => {
                    let s = s.to_owned();
                    self.utf8.clear();
                    self.append(&s);
                }
                Err(e) if e.error_len().is_some() => {
                    self.utf8.clear();
                    self.note = "Invalid UTF-8 input rejected".into();
                }
                _ => (),
            }
            return ComposerEvent::Continue;
        }
        match byte {
            27 => self.escape.push(byte),
            3 | 4 => return ComposerEvent::Cancel,
            10 | 13 if !self.text.is_empty() => return ComposerEvent::Submit,
            10 | 13 => self.note = "Enter text; Esc cancels".into(),
            8 | 127 => {
                self.text.pop();
            }
            0..=31 => self.note = "Control input rejected".into(),
            _ => self.append(&(byte as char).to_string()),
        }
        ComposerEvent::Continue
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composer_decodes_utf8_and_keeps_paste_inert() {
        let mut input = ComposerInput::default();
        for byte in "한글".as_bytes() {
            assert!(matches!(input.feed(*byte), ComposerEvent::Continue));
        }
        input.feed(127);
        assert_eq!(input.text, "한");
        for paste in [
            b"\x1b[200~one\r\ntwo\x1b[201~".as_slice(),
            b"\x1b[200~bad\x1b[31m\x03\x00\x1b[201~",
            b"\x1b[200~\xff\x1b[201~",
        ] {
            for byte in paste {
                assert!(matches!(input.feed(*byte), ComposerEvent::Continue));
            }
            assert_eq!(input.text, "한");
            assert!(!input.note.is_empty());
        }
        let literal = " --한글 '$HOME' `literal` ";
        for byte in format!("\x1b[200~{literal}\x1b[201~").bytes() {
            assert!(matches!(input.feed(byte), ComposerEvent::Continue));
        }
        assert_eq!(input.text, format!("한{literal}"));
        assert!(matches!(input.feed(13), ComposerEvent::Submit));
    }
    #[test]
    fn composer_limits_and_control_sequences() {
        let mut input = ComposerInput::default();
        assert!(matches!(input.feed(13), ComposerEvent::Continue));
        for byte in b"\x1b[A\x1b[3~\x1bOP\x00\t" {
            input.feed(*byte);
        }
        assert!(input.text.is_empty());
        for byte in b"\x1b[200~" {
            input.feed(*byte);
        }
        for _ in 0..COMPOSER_LIMIT + 100 {
            input.feed(b'x');
        }
        for byte in b"\x1b[201~" {
            input.feed(*byte);
        }
        assert!(input.text.is_empty());
        assert!(input.note.contains("rejected"));
        assert!(input.paste.is_none());
        input.append(&"x".repeat(COMPOSER_LIMIT));
        input.append("한");
        assert_eq!(input.text.len(), COMPOSER_LIMIT);
        assert!(!safe_text("invisible\u{202e}"));
        assert!(!safe_text("line\u{2028}break"));
        assert!(safe_text("한글 e\u{301}"));
        assert!(matches!(input.feed(3), ComposerEvent::Cancel));
    }
    #[test]
    fn daylight_saving_calendar() {
        if std::env::var("HERDR_TEST_DST_CHILD").is_err() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "dialog::tests::daylight_saving_calendar"])
                .env("TZ", "America/New_York")
                .env("HERDR_TEST_DST_CHILD", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let now = calendar([2026, 3, 7, 12, 0], true).unwrap();
        let tomorrow = calendar([2026, 3, 8, 9, 0], true).unwrap();
        assert_eq!(tomorrow - now, 20. * 3600.);
        let mut form = Form::new(Some(now));
        form.values = [2026, 3, 8, 1, 59];
        form.index = 4;
        form.adjust(1).unwrap();
        assert_eq!(form.values, [2026, 3, 8, 2, 0]);
        assert!(form.due(now).is_err());
        form.values = [2026, 3, 8, 2, 30];
        assert!(
            form.due(now).is_err(),
            "DST gap must not silently normalize"
        );
        form.values = [2026, 3, 8, 9, 0];
        assert_eq!(form.due(now).unwrap(), tomorrow);
    }
    #[test]
    fn calendar_fields() {
        let mut f = Form {
            values: [2028, 1, 31, 23, 59],
            index: 1,
            draft: None,
        };
        f.adjust(1).unwrap();
        assert_eq!(f.values, [2028, 2, 29, 23, 59]);
        f.index = 0;
        f.adjust(1).unwrap();
        assert_eq!(f.values, [2029, 2, 28, 23, 59]);
        f.index = 4;
        f.adjust(1).unwrap();
        assert_eq!(f.values, [2029, 3, 1, 0, 0]);
        f.index = 3;
        f.digit('2');
        f.digit('5');
        assert!(f.move_field(1).is_err());
        f.backspace();
        f.digit('3');
        f.move_field(1).unwrap();
        assert_eq!(f.values[3], 23);
        f.values = [2026, 2, 30, 9, 0];
        assert!(f.due(0.).is_err());
    }
    #[test]
    fn fitting() {
        assert_eq!(fitted("한글e\u{301}", 6), "한글e\u{301} ");
    }
}
