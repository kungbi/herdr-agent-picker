#![allow(dead_code)]
//! Isolated subprocess/PTY/SQLite fixtures. No global environment mutation.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString, OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{MetadataExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub const ROOT: &str = env!("CARGO_MANIFEST_DIR");
pub const BINARY: &str = env!("CARGO_BIN_EXE_herdr-agent-picker");
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS reminders (id TEXT PRIMARY KEY, scope TEXT NOT NULL, pane_id TEXT NOT NULL, title TEXT NOT NULL, due REAL NOT NULL, context TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', retry_at REAL NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, error TEXT, UNIQUE(scope,pane_id)); CREATE TABLE IF NOT EXISTS unread_reminders (scope TEXT NOT NULL,pane_id TEXT NOT NULL,PRIMARY KEY(scope,pane_id));";
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}
pub fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
pub fn plain(s: &str) -> String {
    regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]")
        .unwrap()
        .replace_all(s, "")
        .replace('\r', "")
}
pub fn styles(s: &str) -> Vec<&str> {
    regex::Regex::new(r"\x1b\[[0-9;:]*m")
        .unwrap()
        .find_iter(s)
        .map(|m| m.as_str())
        .collect()
}
pub fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}
pub fn until(mut test: impl FnMut() -> bool, timeout: Duration) {
    let end = Instant::now() + timeout;
    while !test() {
        assert!(Instant::now() < end, "condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}
pub fn fake_binary() -> PathBuf {
    static BINARY_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BINARY_PATH
        .get_or_init(|| {
            // Use the supplied application path only to choose a shared cache location.
            // A separate target dir avoids the outer Cargo invocation's build lock;
            // Cargo handles freshness and reports the executable's actual location.
            let target = Path::new(BINARY).with_file_name("herdr-agent-picker-test-support");
            let mut c = Command::new(env!("CARGO"));
            c.current_dir(ROOT)
                .args([
                    "build",
                    "--locked",
                    "--offline",
                    "--example",
                    "fake_herdr",
                    "--message-format=json",
                    "--target-dir",
                ])
                .arg(target);
            let o = output(c, Duration::from_secs(180));
            assert!(
                o.status.success(),
                "build fake_herdr:\n{}\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8(o.stdout)
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find_map(|m| {
                    (m["reason"] == "compiler-artifact"
                        && m["target"]["name"] == "fake_herdr"
                        && m["target"]["kind"] == json!(["example"]))
                    .then(|| m["executable"].as_str().map(PathBuf::from))
                    .flatten()
                })
                .expect("Cargo must report the fake_herdr example executable")
        })
        .clone()
}
pub struct Fixture {
    pub root: PathBuf,
    vars: BTreeMap<OsString, OsString>,
    _slot: std::sync::MutexGuard<'static, ()>,
}
impl Fixture {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Serialize setup too: completion ages must be seeded after any queue wait.
        static CASE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let slot = CASE.lock().unwrap_or_else(|e| e.into_inner());
        let base = Path::new(ROOT).join("target/test-tmp");
        fs::create_dir_all(&base).unwrap();
        let root = loop {
            let path = base.join(format!(
                "{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create fixture: {e}"),
            }
        };
        fs::write(root.join("session.sock"), "").unwrap();
        fs::write(
            root.join("config.toml"),
            "[ui]\nagent_panel_sort = \"priority\"\n",
        )
        .unwrap();
        let mut f = Self {
            root,
            vars: BTreeMap::new(),
            _slot: slot,
        };
        for (key, name) in [
            ("HERDR_SOCKET_PATH", "session.sock"),
            ("HERDR_CONFIG_PATH", "config.toml"),
            ("HERDR_PLUGIN_STATE_DIR", "state"),
            ("HOME", "home"),
            ("DATA", "data.json"),
            ("CALLS", "calls"),
            ("FOCUS", "focus"),
            ("EVENTS", "events"),
            ("ATTACHED", "attached"),
            ("RELEASED", "released"),
            ("REFRESH_STARTED", "refresh-started"),
        ] {
            f.env(key, f.path(name));
        }
        fs::create_dir_all(f.path("home")).unwrap();
        // Reuse the validated executable inode on macOS; isolate only chmod tests.
        let fake = f.path("herdr");
        std::os::unix::fs::symlink(fake_binary(), &fake).unwrap();
        f.env("HERDR_BIN_PATH", fake)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("FAKE_MODE", "picker");
        f
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        if key.as_ref() == OsStr::new("FAIL_ATTACH_SPAWN") {
            let path = self.path("herdr");
            if path.is_symlink() {
                fs::remove_file(&path).unwrap();
                fs::copy(fake_binary(), &path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        self.vars.insert(key.as_ref().into(), value.as_ref().into());
        self
    }
    pub fn command(&self) -> Command {
        self.configure(Command::new(BINARY))
    }
    pub fn configure(&self, mut c: Command) -> Command {
        for key in [
            "FAKE_MODE",
            "DATA",
            "CALLS",
            "FOCUS",
            "EVENTS",
            "ATTACHED",
            "RELEASED",
            "CONTENT",
            "TEXT_CONTENT",
            "APP_HISTORY_CONTENT",
            "REJECT_APP_HISTORY",
            "CROSS_TAB_CONTENT",
            "SNAPSHOTS",
            "SNAPSHOT",
            "SLOW_REFRESH",
            "REFRESH_STARTED",
            "FAIL_ATTACH_SPAWN",
            "FAIL_ATTACH",
            "FAIL_READ",
            "PICKER_AGENT_COUNT",
            "PICKER_AGENT_KIND",
            "PICKER_UNREAD_ALARM",
            "PICKER_LONG_PREVIEW",
            "PICKER_PADDED_PREVIEW",
        ] {
            c.env_remove(key);
        }
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("HERDR_") || k.to_string_lossy().starts_with("FZF_")
            {
                c.env_remove(k);
            }
        }
        c.env_remove("ENV")
            .env_remove("BASH_ENV")
            .env_remove("CDPATH");
        c.envs(&self.vars).current_dir(&self.root);
        c
    }
    pub fn write(&self, name: &str, s: &str) {
        let path = self.path(name);
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let tmp = path.with_extension("fixture-tmp");
        fs::write(&tmp, s).unwrap();
        fs::rename(tmp, path).unwrap();
    }
    pub fn write_json(&self, name: &str, v: &Value) {
        self.write(name, &v.to_string());
    }
    pub fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.path("calls"))
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).expect("complete atomic fake argv log"))
            .collect()
    }
    pub fn call_text(&self) -> String {
        self.calls()
            .iter()
            .map(|a| a.join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn events(&self) -> Vec<String> {
        fs::read_to_string(self.path("events"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    pub fn focus(&self) -> Option<String> {
        fs::read_to_string(self.path("focus")).ok()
    }
    pub fn spawn(&self, args: &[&str], rows: u16, cols: u16) -> Pty {
        let mut c = self.command();
        c.args(args);
        Pty::spawn(c, rows, cols)
    }
    pub fn run(&self, args: &[&str]) -> Output {
        let mut c = self.command();
        c.args(args);
        output(c, Duration::from_secs(8))
    }
    pub fn cache_path(&self) -> PathBuf {
        let s = self.vars[OsStr::new("HERDR_SOCKET_PATH")].to_string_lossy();
        let m = fs::metadata(s.as_ref()).unwrap();
        let ns = m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128;
        Path::new(&self.vars[OsStr::new("HERDR_PLUGIN_STATE_DIR")])
            .join("completions")
            .join(format!(
                "{}.json",
                &hash(&format!("{s}:{}:{ns}", m.ino()))[..24]
            ))
    }
    pub fn context(&self) -> Value {
        let s = self.vars[OsStr::new("HERDR_SOCKET_PATH")].to_string_lossy();
        let m = fs::metadata(s.as_ref()).unwrap();
        let ns = m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128;
        json!({"scope":hash(&format!("{s}:{}:{}:{ns}",m.dev(),m.ino())),"socket":s,"config":self.vars[OsStr::new("HERDR_CONFIG_PATH")].to_string_lossy(),"herdr":self.vars[OsStr::new("HERDR_BIN_PATH")].to_string_lossy()})
    }
    pub fn times(&self) -> Value {
        fs::read(self.cache_path())
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or(json!({}))
    }
    pub fn seed_event(&self, pane: &str, status: &str, at: f64) {
        let mut times = self.times();
        let old = &times[pane];
        let mut completed = old["completed_at"].clone();
        let mut pending = false;
        if status == "done"
            && (!matches!(old["status"].as_str(), Some("done" | "idle")) || completed.is_null())
        {
            completed = json!(at);
        }
        if status == "idle" && matches!(old["status"].as_str(), Some("working" | "blocked")) {
            completed = json!(at);
            pending = true;
        }
        if !matches!(status, "done" | "idle") {
            completed = Value::Null;
        }
        times[pane] = json!({"status":status,"completed_at":completed,"pending_done":pending,"observed_at":at});
        let p = self.cache_path();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let tmp = p.with_extension("fixture-tmp");
        fs::write(&tmp, times.to_string()).unwrap();
        fs::rename(tmp, p).unwrap();
    }
    pub fn db(&self, sql: &str) -> Vec<Vec<String>> {
        self.db_values(sql)
            .into_iter()
            .map(|row| {
                row.as_object()
                    .unwrap()
                    .values()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .collect()
            })
            .collect()
    }
    pub fn db_values(&self, sql: &str) -> Vec<Value> {
        let state = Path::new(&self.vars[OsStr::new("HERDR_PLUGIN_STATE_DIR")]);
        fs::create_dir_all(state).unwrap();
        sqlite(&state.join("reminders.sqlite3"), sql)
    }
    pub fn pending(&self) -> Vec<Value> {
        self.db_values(
            "SELECT * FROM reminders WHERE status IN ('pending','delivering') ORDER BY due",
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub fn output(mut c: Command, timeout: Duration) -> Output {
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn command");
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let a = std::thread::spawn(move || {
        let mut v = vec![];
        out.read_to_end(&mut v).unwrap();
        v
    });
    let b = std::thread::spawn(move || {
        let mut v = vec![];
        err.read_to_end(&mut v).unwrap();
        v
    });
    let end = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("command timed out: {c:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: a.join().unwrap(),
        stderr: b.join().unwrap(),
    }
}

pub struct Pty {
    pub output: Vec<u8>,
    master: File,
    child: Child,
    status: Option<ExitStatus>,
}
impl Pty {
    pub fn spawn(mut c: Command, rows: u16, cols: u16) -> Self {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::addr_of_mut!(size),
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        c.stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            output: vec![],
            master,
            child: c.spawn().expect("spawn PTY command"),
            status: None,
        }
    }
    pub fn send(&mut self, b: &[u8]) {
        self.master.write_all(b).unwrap();
    }
    pub fn drain(&mut self, d: Duration) {
        let end = Instant::now() + d;
        while Instant::now() < end {
            let mut p = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = (end
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(30)) as i32;
            unsafe {
                libc::poll(&mut p, 1, ms);
            };
            let mut buf = [0; 65536];
            match self.master.read(&mut buf) {
                Ok(0) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(n) => self.output.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("PTY read: {e}"),
            }
        }
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
    pub fn clear(&mut self) {
        self.output.clear();
    }
    #[track_caller]
    pub fn wait_for(&mut self, marker: &[u8]) {
        self.wait_for_timeout(marker, Duration::from_secs(8));
    }
    #[track_caller]
    pub fn wait_for_timeout(&mut self, marker: &[u8], timeout: Duration) {
        let end = Instant::now() + timeout;
        while !self.output.windows(marker.len()).any(|s| s == marker) {
            assert!(
                Instant::now() < end,
                "missing {:?}\n{}",
                String::from_utf8_lossy(marker),
                self.text()
            );
            self.drain(Duration::from_millis(30));
        }
    }
    #[track_caller]
    pub fn wait_until(&mut self, mut f: impl FnMut() -> bool, d: Duration) {
        let end = Instant::now() + d;
        while !f() {
            assert!(Instant::now() < end, "condition timeout\n{}", self.text());
            self.drain(Duration::from_millis(20));
        }
    }
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let s = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &s) },
            0
        );
    }
    pub fn alive(&mut self) -> bool {
        if self.status.is_none() {
            self.status = self.child.try_wait().unwrap();
        }
        self.status.is_none()
    }
    pub fn finish(&mut self) -> ExitStatus {
        let end = Instant::now() + Duration::from_secs(5);
        while self.alive() {
            assert!(
                Instant::now() < end,
                "PTY child failed to exit\n{}",
                self.text()
            );
            self.drain(Duration::from_millis(25));
        }
        self.drain(Duration::from_millis(20));
        self.status.unwrap()
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn kill(&mut self) {
        if self.status.is_some() {
            return;
        }
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        };
        if self.status.is_none() {
            self.status = Some(self.child.wait().unwrap());
        }
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        self.kill();
    }
}
pub fn lock_available(path: &Path) -> bool {
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open(p: *const libc::c_char, db: *mut *mut libc::c_void) -> i32;
    fn sqlite3_close(db: *mut libc::c_void) -> i32;
    fn sqlite3_busy_timeout(db: *mut libc::c_void, n: i32) -> i32;
    fn sqlite3_exec(
        db: *mut libc::c_void,
        sql: *const libc::c_char,
        cb: Option<
            unsafe extern "C" fn(
                *mut libc::c_void,
                i32,
                *mut *mut libc::c_char,
                *mut *mut libc::c_char,
            ) -> i32,
        >,
        arg: *mut libc::c_void,
        err: *mut *mut libc::c_char,
    ) -> i32;
    fn sqlite3_free(p: *mut libc::c_void);
}
pub fn sqlite(path: &Path, sql: &str) -> Vec<Value> {
    unsafe extern "C" fn row(
        arg: *mut libc::c_void,
        n: i32,
        values: *mut *mut libc::c_char,
        names: *mut *mut libc::c_char,
    ) -> i32 {
        let rows = &mut *(arg as *mut Vec<Value>);
        let mut r = serde_json::Map::new();
        for i in 0..n as usize {
            let key = CStr::from_ptr(*names.add(i)).to_string_lossy().into_owned();
            let p = *values.add(i);
            let v = if p.is_null() {
                Value::Null
            } else {
                let s = CStr::from_ptr(p).to_string_lossy().into_owned();
                s.parse::<f64>().ok().map(|f| json!(f)).unwrap_or(json!(s))
            };
            r.insert(key, v);
        }
        rows.push(Value::Object(r));
        0
    }
    let path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let sql = CString::new(sql).unwrap();
    let mut db = std::ptr::null_mut();
    let mut rows: Vec<Value> = vec![];
    unsafe {
        assert_eq!(sqlite3_open(path.as_ptr(), &mut db), 0);
        sqlite3_busy_timeout(db, 3000);
        let mut err = std::ptr::null_mut();
        let result = sqlite3_exec(
            db,
            sql.as_ptr(),
            Some(row),
            &mut rows as *mut _ as *mut _,
            &mut err,
        );
        let message = if err.is_null() {
            String::new()
        } else {
            let s = CStr::from_ptr(err).to_string_lossy().into_owned();
            sqlite3_free(err.cast());
            s
        };
        sqlite3_close(db);
        assert_eq!(result, 0, "SQLite {message}");
    }
    rows
}
pub fn sql(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
pub fn local_time(at: f64) -> libc::tm {
    let t = at as libc::time_t;
    let mut tm = std::mem::MaybeUninit::uninit();
    unsafe {
        assert!(!libc::localtime_r(&t, tm.as_mut_ptr()).is_null());
        tm.assume_init()
    }
}
pub fn timestamp(mut tm: libc::tm) -> f64 {
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as f64 }
}
pub fn local_format(at: f64, fmt: &str) -> String {
    let tm = local_time(at);
    let fmt = CString::new(fmt).unwrap();
    let mut b = [0u8; 128];
    let n = unsafe { libc::strftime(b.as_mut_ptr().cast(), b.len(), fmt.as_ptr(), &tm) };
    String::from_utf8(b[..n].to_vec()).unwrap()
}

pub struct Screen {
    cells: Vec<Vec<char>>,
    pub frame: Vec<Vec<char>>,
    colors: Vec<Vec<Option<Vec<usize>>>>,
    pub frame_colors: Vec<Vec<Option<Vec<usize>>>>,
    foreground: Option<Vec<usize>>,
    cursor: (usize, usize),
    saved: (usize, usize),
    pending: Vec<u8>,
    csi: regex::Regex,
    row_pattern: regex::Regex,
}

impl Screen {
    pub fn new(rows: usize, columns: usize) -> Self {
        let cells = vec![vec![' '; columns]; rows];
        Self {
            frame: cells.clone(),
            cells,
            colors: vec![vec![None; columns]; rows],
            frame_colors: vec![vec![None; columns]; rows],
            foreground: None,
            cursor: (0, 0),
            saved: (0, 0),
            pending: Vec::new(),
            csi: regex::Regex::new(r"^\x1b\[([0-?]*)([ -/]*)([@-~])").unwrap(),
            row_pattern: regex::Regex::new(r"ROW_[0-9]{2}(?:_EDIT)?").unwrap(),
        }
    }

    pub fn resize(&mut self, rows: usize, columns: usize) {
        for grid in [&mut self.cells, &mut self.frame] {
            grid.resize(rows, vec![' '; columns]);
            for row in grid {
                row.resize(columns, ' ');
            }
        }
        for grid in [&mut self.colors, &mut self.frame_colors] {
            grid.resize(rows, vec![None; columns]);
            for row in grid {
                row.resize(columns, None);
            }
        }
        self.cursor = (self.cursor.0.min(rows - 1), self.cursor.1.min(columns - 1));
        self.saved = (self.saved.0.min(rows - 1), self.saved.1.min(columns - 1));
    }

    pub fn feed(&mut self, data: &[u8]) {
        use unicode_width::UnicodeWidthChar;
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(data);
        let mut bytes = bytes.as_slice();
        while !bytes.is_empty() {
            let consumed = if bytes.starts_with(b"\x1b[") {
                let Some(end) = bytes[2..]
                    .iter()
                    .position(|byte| (b'@'..=b'~').contains(byte))
                else {
                    break;
                };
                let length = end + 3;
                let sequence = std::str::from_utf8(&bytes[..length]).unwrap();
                let captures = self
                    .csi
                    .captures(sequence)
                    .unwrap_or_else(|| panic!("bad CSI {sequence:?}"));
                self.control(&captures[1], captures[3].as_bytes()[0]);
                length
            } else if bytes[0] == 27 {
                let Some(escape) = bytes.get(1) else { break };
                match escape {
                    b']' => {
                        let Some(end) = (2..bytes.len())
                            .find(|&i| bytes[i] == 7 || bytes[i..].starts_with(b"\x1b\\"))
                        else {
                            break;
                        };
                        end + if bytes[end] == 7 { 1 } else { 2 }
                    }
                    b'7' => {
                        self.saved = self.cursor;
                        2
                    }
                    b'8' => {
                        self.cursor = self.saved;
                        2
                    }
                    b'(' | b')' if bytes.len() >= 3 => 3,
                    b'(' | b')' => break,
                    other => panic!("unsupported escape {other:?}"),
                }
            } else {
                let width = match bytes[0] {
                    0..=127 => 1,
                    194..=223 => 2,
                    224..=239 => 3,
                    240..=244 => 4,
                    _ => panic!("invalid UTF-8 in terminal output: {bytes:?}"),
                };
                if bytes.len() < width {
                    break;
                }
                let ch = std::str::from_utf8(&bytes[..width])
                    .unwrap()
                    .chars()
                    .next()
                    .unwrap();
                let (row, col) = &mut self.cursor;
                match ch {
                    '\r' => *col = 0,
                    '\n' => *row = (*row + 1).min(self.cells.len() - 1),
                    '\x08' => *col = col.saturating_sub(1),
                    _ => {
                        if let Some(width @ 1..) = ch.width() {
                            self.cells[*row][*col] = ch;
                            self.colors[*row][*col] = self.foreground.clone();
                            *col = (*col + width).min(self.cells[0].len() - 1);
                        }
                    }
                }
                width
            };
            bytes = &bytes[consumed..];
        }
        self.pending.extend_from_slice(bytes);
    }

    pub fn control(&mut self, args: &str, action: u8) {
        if args.starts_with('?') {
            if args == "?7" && action == b'h' {
                // fzf restores auto-wrap at the end of a repaint.
                self.frame.clone_from(&self.cells);
                self.frame_colors.clone_from(&self.colors);
            }
            return;
        }
        let mut values = args
            .split(';')
            .map(|value| value.parse::<usize>().unwrap_or(0));
        let code = values.next().unwrap_or(0);
        let n = code.max(1);
        let (row, col) = &mut self.cursor;
        let columns = self.cells[0].len();
        match action {
            b'A' => *row = row.saturating_sub(n),
            b'B' => *row += n,
            b'C' => *col += n,
            b'D' => *col = col.saturating_sub(n),
            b'H' | b'f' => {
                *row = n - 1;
                *col = values.next().unwrap_or(0).max(1) - 1;
            }
            b'G' => *col = n - 1,
            b'd' => *row = n - 1,
            b'J' => match code {
                0 => {
                    self.cells[*row][*col..].fill(' ');
                    for line in &mut self.cells[*row + 1..] {
                        line.fill(' ');
                    }
                }
                2 | 3 => {
                    for line in &mut self.cells {
                        line.fill(' ');
                    }
                }
                _ => panic!("unsupported erase: {args}"),
            },
            b'K' | b'X' => {
                let (start, end) = match (action, code) {
                    (b'X', _) => (*col, (*col + n).min(columns)),
                    (b'K', 1) => (0, *col + 1),
                    (b'K', 2) => (0, columns),
                    _ => (*col, columns),
                };
                self.cells[*row][start..end].fill(' ');
            }
            b's' => self.saved = (*row, *col),
            b'u' => (*row, *col) = self.saved,
            b'm' => {
                let codes: Vec<usize> = args.split(';').map(|v| v.parse().unwrap_or(0)).collect();
                let mut i = 0;
                while i < codes.len() {
                    match codes[i] {
                        0 | 39 => self.foreground = None,
                        30..=37 | 90..=97 => self.foreground = Some(vec![codes[i]]),
                        38 if codes.get(i + 1) == Some(&5) && i + 2 < codes.len() => {
                            self.foreground = Some(codes[i..=i + 2].to_vec());
                            i += 2;
                        }
                        38 if codes.get(i + 1) == Some(&2) && i + 4 < codes.len() => {
                            self.foreground = Some(codes[i..=i + 4].to_vec());
                            i += 4;
                        }
                        48 if codes.get(i + 1) == Some(&5) && i + 2 < codes.len() => i += 2,
                        48 if codes.get(i + 1) == Some(&2) && i + 4 < codes.len() => i += 4,
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'h' | b'l' | b'n' | b't' | b'q' => {}
            _ => panic!("unsupported CSI: {args} {}", char::from(action)),
        }
        *row = (*row).min(self.cells.len() - 1);
        *col = (*col).min(columns - 1);
    }

    pub fn preview(&self) -> Vec<String> {
        let text = self
            .frame
            .iter()
            .map(|row| row[43..97].iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        self.row_pattern
            .find_iter(&text)
            .map(|row| row.as_str().to_owned())
            .collect()
    }

    pub fn pump(&mut self, picker: &mut Pty) {
        picker.drain(Duration::from_millis(30));
        self.feed(&picker.output);
        picker.clear();
    }
}
