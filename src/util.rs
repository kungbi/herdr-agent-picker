use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
pub fn signals() {
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as usize);
        libc::signal(libc::SIGINT, stop as *const () as usize);
    }
}
pub fn stopped() -> bool {
    STOP.load(Ordering::Relaxed)
}
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}
pub fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
pub fn config() -> PathBuf {
    env::var_os("HERDR_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config/herdr/config.toml"))
}
pub fn state_dir() -> PathBuf {
    env::var_os("HERDR_PLUGIN_STATE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/state"))
                .join("herdr-agent-picker")
        })
}
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub fn herdr() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}
pub fn executable() -> Result<String> {
    Ok(env::current_exe()?.to_string_lossy().into_owned())
}
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
pub fn shell(args: &[&str]) -> String {
    args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
}
pub fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub fn label(s: &str) -> String {
    static UNPRINTABLE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    UNPRINTABLE
        .get_or_init(|| regex::Regex::new(r"[\p{C}\p{Zl}\p{Zp}]").unwrap())
        .replace_all(s, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn command_output(command: &mut Command, timeout: Duration) -> Result<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Drain both pipes concurrently; a large terminal snapshot must not deadlock wait.
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let read = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let stdout = read(Box::new(out));
    let stderr = read(Box::new(err));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline || stopped() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = stdout.join().map_err(|_| "Output reader failed")??;
    let error = stderr.join().map_err(|_| "Error reader failed")??;
    let error_text = String::from_utf8_lossy(&error);
    let error_message = serde_json::from_slice::<Value>(&error)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| error_text.into_owned());
    match status {
        Some(status) if status.success() => Ok(String::from_utf8_lossy(&output).into_owned()),
        Some(status) => Err(format!("Command failed: {status}: {}", label(&error_message)).into()),
        None => Err("Command timed out or interrupted".into()),
    }
}
pub fn request(args: &[&str]) -> Result<Value> {
    let text = command_output(Command::new(herdr()).args(args), Duration::from_secs(3))?;
    let response: Value = serde_json::from_str(&text)?;
    if let Some(error) = response.get("error") {
        return Err(error["message"]
            .as_str()
            .unwrap_or("Herdr request failed")
            .into());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "Missing Herdr result".into())
}
pub fn exec_herdr(args: &[&str]) -> Result<()> {
    Err(Command::new(herdr()).args(args).exec().into())
}
pub fn lock(path: &Path, nonblocking: bool) -> Result<Option<File>> {
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    use std::os::fd::AsRawFd;
    if unsafe {
        libc::flock(
            f.as_raw_fd(),
            libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 },
        )
    } != 0
    {
        let e = std::io::Error::last_os_error();
        if nonblocking && e.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(e.into());
    }
    Ok(Some(f))
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(tmp)?;
    file.write_all(bytes)?;
    fs::rename(tmp, path)?;
    Ok(())
}
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct TempDir(pub PathBuf);
impl TempDir {
    pub fn new() -> Result<Self> {
        #[cfg(test)]
        let base = {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
            fs::create_dir_all(&path)?;
            path
        };
        #[cfg(not(test))]
        let base = env::temp_dir();
        for _ in 0..100 {
            let path = base.join(format!(
                "herdr-agent-picker-{}-{}-{}",
                std::process::id(),
                (now() * 1e9) as u128,
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err("Could not create temporary directory".into())
    }
}
use std::os::unix::fs::DirBuilderExt;
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub fn local(timestamp: f64) -> libc::tm {
    unsafe {
        let t = timestamp as libc::time_t;
        let mut tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}
pub fn date(timestamp: f64) -> String {
    let t = local(timestamp);
    format!(
        "{:04}-{:02}-{:02}",
        t.tm_year + 1900,
        t.tm_mon + 1,
        t.tm_mday
    )
}
pub fn date_time(timestamp: f64) -> String {
    let t = local(timestamp);
    format!("{} {:02}:{:02}", date(timestamp), t.tm_hour, t.tm_min)
}
pub fn calendar(values: [i32; 5], strict: bool) -> Result<f64> {
    let [y, m, d, h, min] = values;
    if !(1..=9999).contains(&y) {
        return Err("Year: use 1-9999.".into());
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = m - 1;
    tm.tm_mday = d;
    tm.tm_hour = h;
    tm.tm_min = min;
    tm.tm_isdst = -1;
    let timestamp = unsafe { libc::mktime(&mut tm) } as f64;
    let t = local(timestamp);
    if strict
        && [
            t.tm_year + 1900,
            t.tm_mon + 1,
            t.tm_mday,
            t.tm_hour,
            t.tm_min,
        ] != values
    {
        return Err("Invalid date or nonexistent local time. Choose another time.".into());
    }
    Ok(timestamp)
}
pub fn which(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH").unwrap_or_default();
    let candidates = if name.contains('/') {
        vec![PathBuf::from(name)]
    } else {
        env::split_paths(&paths).map(|p| p.join(name)).collect()
    };
    candidates.into_iter().find(|p| {
        fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}
pub fn check() -> Result<()> {
    println!("herdr-agent-picker {} (Rust)", env!("CARGO_PKG_VERSION"));
    let re = regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)")?;
    for (name, min) in [(herdr(), (0, 9, 1)), ("fzf".into(), (0, 73, 1))] {
        let path = which(&name).ok_or_else(|| format!("Missing dependency: {name}"))?;
        let output = command_output(Command::new(&path).arg("--version"), Duration::from_secs(3))?;
        let c = re.captures(&output).ok_or("Invalid dependency version")?;
        let version = (
            c[1].parse::<u32>()?,
            c[2].parse::<u32>()?,
            c[3].parse::<u32>()?,
        );
        if version < min {
            return Err(format!("{name} requires {}.{}.{} or newer", min.0, min.1, min.2).into());
        }
        println!("{} ({})", output.trim(), path.display());
    }
    println!("Dependencies OK");
    Ok(())
}

pub fn tty_path(fd: i32) -> Option<std::path::PathBuf> {
    let mut bytes = vec![0u8; 1024];
    if unsafe { libc::ttyname_r(fd, bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return None;
    }
    let n = bytes.iter().position(|b| *b == 0)?;
    Some(std::path::PathBuf::from(
        String::from_utf8_lossy(&bytes[..n]).into_owned(),
    ))
}
