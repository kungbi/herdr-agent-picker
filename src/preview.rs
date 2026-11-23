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

pub fn clean(text: &str) -> String {
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
pub fn snapshot(pane: &str) -> Result<bool> {
    if pane.is_empty() || pane == "@section" {
        return Ok(false);
    }
    let output = command_output(
        Command::new(herdr()).args([
            "pane",
            "read",
            pane,
            "--source",
            "recent-unwrapped",
            "--lines",
            "30",
            "--format",
            "ansi",
        ]),
        Duration::from_secs(3),
    );
    match output {
        Ok(text) => {
            print!("{}", clean(&text));
            Ok(true)
        }
        Err(_) => {
            println!("Preview unavailable");
            Ok(false)
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
            &mut size,
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
    // Unknown origin is deliberately read-only too: guessing could resize the
    // visible pane. Keep the lock so a previous attachment is reaped first.
    if origin.is_empty() || pane == origin {
        while !stopped() {
            print!("\x1b[2J\x1b[H");
            if !snapshot(pane)? {
                return Ok(());
            }
            std::io::stdout().flush()?;
            let end = Instant::now() + Duration::from_secs(1);
            while Instant::now() < end && !stopped() {
                std::thread::sleep(Duration::from_millis(100));
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
    while !stopped() {
        print!("\x1b[2J\x1b[H");
        if !snapshot(pane)? {
            return Ok(());
        }
        std::io::stdout().flush()?;
        let end = Instant::now() + Duration::from_secs(1);
        while Instant::now() < end && !stopped() {
            if attached.child.try_wait()?.is_some() {
                return Err("Terminal attachment ended".into());
            }
            if poll(attached.master.as_raw_fd(), 100) {
                let n = attached.master.read(&mut bytes)?;
                if n == 0 {
                    return Err("Terminal attachment ended".into());
                }
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
    if live_inner(pane, path, origin).is_err() && !stopped() {
        println!("Preview unavailable");
        let _ = std::io::stdout().flush();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
