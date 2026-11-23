use crate::util::*;
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn fifo(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn read_line(file: &mut File) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        match file.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => return Ok(String::from_utf8(bytes)?),
            Ok(1) => bytes.push(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
            _ => return Err("Preview connection ended".into()),
        }
        if bytes.len() > 4096 || Instant::now() >= deadline {
            return Err("Preview handshake unavailable".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn open(path: &Path, pane: &str) -> Result<()> {
    // This binding remains installed during search: acknowledge every literal i
    // so the relay need not guess fzf's mode or race a fixed sleep interval.
    let search = std::env::var("FZF_INPUT_STATE").as_deref() != Ok("hidden");
    let target = (!search && !pane.is_empty() && pane != "@section").then_some(pane);
    atomic_write(
        &path.with_extension("request"),
        serde_json::to_string(&target)?.as_bytes(),
    )?;
    if search {
        println!("put(i)");
    } else if target.is_some() {
        println!("preview-bottom+change-preview-label( Input: Esc returns to picker )");
    }
    Ok(())
}

struct Connection {
    status: File,
    keys: File,
    _lock: File,
}
impl Connection {
    fn open(path: &Path, pane: &str) -> Result<Self> {
        let root = path.with_extension("input");
        let lock = lock(&root.join("client-lock"), true)?.ok_or("Preview input is busy")?;
        let mut status = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(root.join("status"))?;
        let keys = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(root.join("keys"))?;
        let mut stale = [0; 1024];
        while status.read(&mut stale).is_ok_and(|n| n > 0) {}
        atomic_write(&root.join("hello"), serde_json::to_string(pane)?.as_bytes())?;
        let answer = read_line(&mut status)?;
        if answer != "READY" {
            return Err(answer.into());
        }
        Ok(Self {
            status,
            keys,
            _lock: lock,
        })
    }
}

#[derive(Default)]
struct Keys {
    escape: Option<Instant>,
    sequence: Vec<u8>,
    pasted: bool,
    tail: Vec<u8>,
}
impl Keys {
    fn forward(&mut self, b: u8, out: &mut impl Write) -> Result<()> {
        if self.escape.take().is_some() {
            out.write_all(&[27, b])?;
            self.sequence = vec![27, b];
        } else if b == 27 && !self.pasted {
            self.escape = Some(Instant::now());
            return Ok(());
        } else {
            out.write_all(&[b])?;
            if !self.sequence.is_empty() {
                self.sequence.push(b);
            }
        }
        self.tail.push(b);
        if self.tail.len() > 6 {
            self.tail.remove(0);
        }
        if self.sequence == b"\x1b[200~" {
            self.pasted = true;
        }
        if self.tail == b"\x1b[201~" {
            self.pasted = false;
        }
        if self.sequence.len() >= 3 && (0x40..=0x7e).contains(&b) {
            self.sequence.clear();
        }
        Ok(())
    }
    fn escaped(&self) -> bool {
        self.escape
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(40))
    }
}

// fzf keeps its own controlling PTY. The outer tty is relayed byte-for-byte in
// normal mode; input mode routes to the existing preview attachment instead.
// fzf remains free to paint its preview, but never receives interactive keys.
pub fn run(
    mut command: std::process::Command,
    content: &[u8],
    path: &Path,
) -> Result<std::process::Output> {
    use std::{
        os::{fd::FromRawFd, unix::process::CommandExt},
        process::Stdio,
    };
    signals();
    let mut tty = crate::dialog::Dialog::new()?;
    let mut master = -1;
    let mut slave = -1;
    let (columns, rows) = tty.size();
    let mut size = libc::winsize {
        ws_col: columns as u16,
        ws_row: rows as u16,
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
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    command
        .env("HERDR_AGENT_PICKER_INPUT_PATH", path)
        .env(
            "HERDR_AGENT_PICKER_TTY_PATH",
            tty_path(slave.as_raw_fd()).ok_or("Missing fzf tty")?,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(slave.try_clone()?));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(2, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let cleanup_master = master.try_clone()?;
    let cleanup_tty = tty.tty.try_clone()?;
    let child = command.spawn()?;
    drop(slave);
    // Ensure every error path reaps fzf and its preview. fzf's guard separately
    // owns and reaps terminal attach after preview/control-pipe disappearance.
    struct Reap(std::process::Child, File, File);
    impl Drop for Reap {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGTERM);
            }
            // fzf owns separate preview process groups. Keep draining its PTY
            // during cancellation: macOS can block process exit on tty output.
            let start = Instant::now();
            let mut forced = false;
            let mut bytes = [0; 65536];
            loop {
                let mut ready = libc::pollfd {
                    fd: self.1.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut ready, 1, 10) } > 0 {
                    if let Ok(n) = self.1.read(&mut bytes) {
                        let _ = self.2.write_all(&bytes[..n]);
                    }
                }
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                if !forced && start.elapsed() >= Duration::from_secs(2) {
                    let _ = self.0.kill();
                    forced = true;
                }
                if start.elapsed() >= Duration::from_secs(4) {
                    return;
                }
            }
        }
    }
    let mut child = Reap(child, cleanup_master, cleanup_tty);
    child.0.stdin.take().unwrap().write_all(content)?;
    let stdout = child.0.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let mut out = stdout;
        out.read_to_end(&mut data).map(|_| data)
    });
    let request = path.with_extension("request");
    let marker = path.with_extension("active");
    let mut connection: Option<Connection> = None;
    let mut error: Option<String> = None;
    let mut keys = Keys::default();
    let mut waiting: Option<Instant> = None;
    let mut notice_refresh = Instant::now();
    let mut normal_paste = false;
    let mut normal_tail = Vec::new();
    let mut normal_sequence = Vec::new();
    let mut normal_escape = Instant::now();
    let mut bytes = [0; 65536];
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if stopped() {
            return Err("Picker interrupted".into());
        }
        tty.raw()?;
        let (cols, lines) = tty.size();
        if (cols as u16, lines as u16) != (size.ws_col, size.ws_row) {
            if connection.take().is_some() {
                error = Some("Preview resized; re-enter input after Esc".into());
            }
            size.ws_col = cols as u16;
            size.ws_row = lines as u16;
            unsafe {
                libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size);
            }
        }
        if let Ok(data) = fs::read(&request) {
            fs::remove_file(&request)?;
            let pending = waiting.take().is_some();
            let pane: Option<String> = serde_json::from_slice(&data)?;
            if let Some(pane) = pane.filter(|_| pending) {
                atomic_write(&marker, b"active")?;
                match Connection::open(path, &pane) {
                    Ok(c) => {
                        connection = Some(c);
                        error = None;
                        keys = Keys::default();
                    }
                    Err(e) => {
                        error = Some(e.to_string());
                    }
                }
            }
        }
        if waiting.is_some_and(|at| at.elapsed() > Duration::from_secs(3)) {
            waiting = None;
            error = Some("Input mode handshake timed out".into());
        }
        if let Some(c) = connection.as_mut() {
            match c.status.read(&mut bytes) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                Ok(n) => {
                    error = Some(if n == 0 {
                        "Preview attachment ended".into()
                    } else {
                        String::from_utf8_lossy(&bytes[..n]).into_owned()
                    });
                    connection = None;
                }
                Err(e) => {
                    error = Some(e.to_string());
                    connection = None;
                }
            }
        }
        let mut events = [
            libc::pollfd {
                fd: tty.tty.as_raw_fd(),
                events: if waiting.is_none() { libc::POLLIN } else { 0 },
                revents: 0,
            },
            libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(events.as_mut_ptr(), 2, 5) } < 0 {
            continue;
        }
        if events[1].revents != 0 {
            match master.read(&mut bytes) {
                Ok(n) if n > 0 => tty.tty.write_all(&bytes[..n])?,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => (),
                _ => (),
            }
        }
        if let Some(message) = &error {
            if events[1].revents != 0 || Instant::now() >= notice_refresh {
                tty.input_notice(message)?;
                notice_refresh = Instant::now() + Duration::from_millis(100);
            }
        }
        // shortcut: gaps over 40ms can split a key sequence; use framed input if available.
        if !normal_sequence.is_empty()
            && events[0].revents == 0
            && normal_escape.elapsed() >= Duration::from_millis(40)
        {
            master.write_all(&normal_sequence)?;
            normal_sequence.clear();
        }
        if events[0].revents != 0 {
            let mut byte = [0];
            if tty.tty.read(&mut byte)? == 0 {
                return Err("Picker tty ended".into());
            }
            if let Some(c) = connection.as_mut() {
                if let Err(e) = keys.forward(byte[0], &mut c.keys) {
                    error = Some(e.to_string());
                    connection = None;
                }
            } else if error.is_some() {
                // Continue recognizing paste framing after a failure so a
                // pasted ESC cannot return to fzf and leak the remaining bytes.
                keys.forward(byte[0], &mut std::io::sink())?;
            } else {
                let literal_i = byte[0] == b'i' && !normal_paste && normal_sequence.is_empty();
                if byte[0] == 27 {
                    if !normal_sequence.is_empty() {
                        master.write_all(&normal_sequence)?;
                    }
                    normal_sequence = vec![27];
                    normal_escape = Instant::now();
                } else if !normal_sequence.is_empty() {
                    normal_sequence.push(byte[0]);
                    normal_escape = Instant::now();
                    if (normal_sequence.len() == 2 && !b"[O".contains(&byte[0]))
                        || (normal_sequence.len() >= 3 && (0x40..=0x7e).contains(&byte[0]))
                        || normal_sequence.len() >= 4096
                    {
                        master.write_all(&normal_sequence)?;
                        normal_sequence.clear();
                    }
                } else {
                    master.write_all(&byte)?;
                }
                normal_tail.push(byte[0]);
                if normal_tail.len() > 6 {
                    normal_tail.remove(0);
                }
                if normal_tail == b"\x1b[200~" {
                    normal_paste = true;
                }
                if normal_tail == b"\x1b[201~" {
                    normal_paste = false;
                }
                if literal_i {
                    waiting = Some(Instant::now());
                }
            }
        }
        if keys.escaped() {
            connection = None;
            error = None;
            keys = Keys::default();
            let _ = fs::remove_file(&marker);
            // Ask fzf to repaint the error line without changing selection.
            master.write_all(b"\x0c")?;
        }
    };
    let _ = fs::remove_file(marker);
    // Drain the final alternate-screen/termios cleanup already written by fzf.
    let mut p = libc::pollfd {
        fd: master.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    while unsafe { libc::poll(&mut p, 1, 0) } > 0 {
        match master.read(&mut bytes) {
            Ok(n) if n > 0 => tty.tty.write_all(&bytes[..n])?,
            _ => break,
        }
    }
    Ok(std::process::Output {
        status,
        stdout: reader.join().map_err(|_| "fzf output reader failed")??,
        stderr: Vec::new(),
    })
}

// Private FIFOs avoid a listener socket or background shell. Opening the key
// writer pins one preview generation's inode; a replacement cannot inherit keys.
pub struct Input {
    keys: File,
    status: File,
    path: PathBuf,
    target: Option<Target>,
}
impl Drop for Input {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
impl Input {
    pub fn new(lock: &Path) -> Result<Self> {
        let path = lock.with_extension("input");
        // Only the preview lock's owner may replace endpoints from a dead preview.
        let _ = fs::remove_dir_all(&path);
        private_dir(&path)?;
        fifo(&path.join("keys"))?;
        fifo(&path.join("status"))?;
        let keys = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path.join("keys"))?;
        let status = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path.join("status"))?;
        Ok(Self {
            keys,
            status,
            path,
            target: None,
        })
    }
    pub fn active(&self) -> bool {
        self.target.is_some()
    }
    pub fn update(
        &mut self,
        pane: &str,
        terminal: Option<&str>,
        master: Option<&mut File>,
    ) -> Result<()> {
        let mut bytes = [0; 4096];
        // Control is separate from raw bytes: a quick Esc/i re-entry must never
        // be mistaken for target input while an old FIFO writer is disappearing.
        if let Ok(hello) = fs::read(self.path.join("hello")) {
            fs::remove_file(self.path.join("hello"))?;
            self.target = None;
            // Discard any unconsumed bytes from the previous input session. The
            // new client cannot write until it receives this handshake's READY.
            while self.keys.read(&mut bytes).is_ok_and(|n| n > 0) {}
            let check = (|| -> Result<Target> {
                if hello.len() > 4096 {
                    return Err("Invalid preview handshake".into());
                }
                let requested: String = serde_json::from_slice(&hello)?;
                if requested != pane {
                    return Err("Preview selection changed; input refused".into());
                }
                let terminal = terminal
                    .ok_or("Current or unknown origin is protected from attachment resize")?;
                let target = Target::read(pane)?;
                if target.terminal != terminal {
                    return Err("Terminal was replaced; input refused".into());
                }
                Ok(target)
            })();
            match check {
                Ok(target) => {
                    self.status.write_all(b"READY\n")?;
                    self.target = Some(target);
                }
                Err(error) => {
                    writeln!(self.status, "{}", label(&error.to_string()))?;
                }
            }
            return Ok(());
        }
        let n = match self.keys.read(&mut bytes) {
            Ok(0) => {
                self.target = None;
                return Ok(());
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
            Err(e) => return Err(e.into()),
        };
        if self.target.is_none() {
            return Ok(());
        }
        let result = (|| -> Result<()> {
            let target = self.target.as_ref().unwrap();
            // Check while idle and immediately before each delivered batch.
            // Herdr has no atomic identity-conditioned terminal write.
            if *target != Target::read(pane)? {
                return Err("Agent exited or was replaced".into());
            }
            if n > 0 {
                master.ok_or("Attachment ended")?.write_all(&bytes[..n])?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = writeln!(self.status, "Input stopped: {}", label(&error.to_string()));
            self.target = None;
            return Err(error);
        }
        Ok(())
    }
}

// Deliberately exclude mutable labels, status, revision and list position.
#[derive(Debug, PartialEq)]
pub struct Target {
    pane: String,
    kind: String,
    pub terminal: String,
    session: Value,
    name: String,
    foreground: Value,
}
impl Target {
    pub fn read(pane: &str) -> Result<Self> {
        let agent = request(&["agent", "get", pane])?["agent"].clone();
        let processes = request(&["pane", "process-info", "--pane", pane])?;
        Self::from_values(pane, &agent, &processes["process_info"])
    }

    fn from_values(pane: &str, agent: &Value, processes: &Value) -> Result<Self> {
        let kind = string(agent, "agent");
        let terminal = string(agent, "terminal_id");
        let group = processes["foreground_process_group_id"]
            .as_u64()
            .filter(|pid| *pid > 0);
        let shell = processes["shell_pid"].as_u64().filter(|pid| *pid > 0);
        let foreground = processes["foreground_processes"].as_array();
        if string(agent, "pane_id") != pane
            || string(processes, "pane_id") != pane
            || kind.is_empty()
            || terminal.is_empty()
            || group.is_none()
            || shell.is_none()
            || group == shell
            || agent["launch_pending"] == true
            || agent["screen_detection_skipped"] == true
        {
            return Err(
                "Agent is gone or its foreground identity cannot be verified. Input stopped."
                    .into(),
            );
        }
        let foreground =
            foreground.ok_or("Cannot verify agent foreground process. Input stopped.")?;
        let mut identity = vec![];
        let mut recognized = false;
        for process in foreground {
            let pid = process["pid"]
                .as_u64()
                .filter(|pid| *pid > 0)
                .ok_or("Missing foreground PID. Input stopped.")?;
            if Some(pid) == shell {
                return Err("Shell is in the foreground. Input stopped.".into());
            }
            // A retained agent may be suspended while a different command runs.
            // Require the agent executable/package in the foreground argv as well.
            let commands = [string(process, "name"), string(process, "argv0")];
            let matches = |s: &str| {
                let name = Path::new(s)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                name == kind.to_lowercase()
                    || name.starts_with(&format!("{}-", kind.to_lowercase()))
            };
            recognized |= commands.into_iter().any(matches)
                || process["argv"].as_array().is_some_and(|args| {
                    args.iter()
                        .filter_map(Value::as_str)
                        .filter(|s| s.starts_with('/'))
                        .any(|s| s.split('/').any(matches))
                });
            identity.push(json!({"pid": pid, "name": process["name"], "argv0": process["argv0"], "argv": process["argv"]}));
        }
        if !recognized {
            return Err("Agent is not verifiably in the foreground. Input stopped.".into());
        }
        identity.sort_by_key(|p| p["pid"].as_u64());
        Ok(Self {
            pane: pane.into(),
            kind: kind.into(),
            terminal: terminal.into(),
            session: agent["agent_session"].clone(),
            name: string(agent, "name").into(),
            foreground: json!({"group":group, "shell":shell, "processes":identity}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_identity_ignores_list_state_but_detects_replacement() {
        let mut agent = json!({"pane_id":"w1:p1", "agent":"claude", "terminal_id":"t1", "name":"writer",
            "agent_session":{"value":"session-1"}});
        let mut process = json!({"pane_id":"w1:p1", "shell_pid":10, "foreground_process_group_id":20,
            "foreground_processes":[{"pid":20,"name":"node","argv":["node","/opt/claude-code/cli.js"]}]});
        let target = Target::from_values("w1:p1", &agent, &process).unwrap();
        agent["agent_status"] = json!("done");
        agent["title"] = json!("New title");
        agent["focused"] = json!(false);
        agent["revision"] = json!(99);
        assert_eq!(
            target,
            Target::from_values("w1:p1", &agent, &process).unwrap()
        );
        process["foreground_processes"][0]["pid"] = json!(21);
        assert_ne!(
            target,
            Target::from_values("w1:p1", &agent, &process).unwrap()
        );
        process["foreground_process_group_id"] = json!(10);
        assert!(Target::from_values("w1:p1", &agent, &process).is_err());
        process["foreground_process_group_id"] = json!(20);
        process["foreground_processes"][0]["name"] = json!("sleep");
        process["foreground_processes"][0]["argv"] = json!(["sleep", "10"]);
        assert!(Target::from_values("w1:p1", &agent, &process).is_err());
        assert!(Target::from_values("w1:p1", &agent, &Value::Null).is_err());
    }
}
