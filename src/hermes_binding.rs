use crate::util::{home, request};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

fn private_file(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    (meta.is_file()
        && meta.uid() == unsafe { libc::geteuid() }
        && meta.mode() & 0o077 == 0
        && meta.len() <= 4096)
        .then_some(file)
}
fn json(file: &File) -> Option<Value> {
    serde_json::from_reader(file.take(4097)).ok()
}
fn active(path: &Path, pane: &str) -> Option<String> {
    let lease = private_file(path)?;
    // A file left by a killed launcher is not a binding, even if the OS reuses its PID.
    if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0
        || std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock
    {
        return None;
    }
    let binding = json(&lease)?;
    if binding["pane"].as_str()? != pane {
        return None;
    }
    session_marker(Path::new(binding["marker"].as_str()?))
}

fn session_marker(marker: &Path) -> Option<String> {
    if !marker.is_absolute()
        || !marker
            .file_name()?
            .to_str()?
            .starts_with("hermes-tui-active-session-")
    {
        return None;
    }
    let value = json(&private_file(marker)?)?;
    let id = value["session_id"].as_str()?;
    (!id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c)))
    .then(|| id.to_owned())
}

#[cfg(target_os = "macos")]
fn legacy_marker(pid: i32, entry: &Path, profile: &Path) -> Option<PathBuf> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0;
    let mut read = |buf, size: &mut usize| unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, buf, size, std::ptr::null_mut(), 0)
    };
    if read(std::ptr::null_mut(), &mut size) != 0 || !(5..=4 * 1024 * 1024).contains(&size) {
        return None;
    }
    let mut bytes = vec![0u8; size];
    if read(bytes.as_mut_ptr().cast(), &mut size) != 0 {
        return None;
    }
    parse_marker(bytes.get(..size)?, entry, profile)
}

#[cfg(any(test, target_os = "macos"))]
fn parse_marker(bytes: &[u8], entry: &Path, profile: &Path) -> Option<PathBuf> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let argc = usize::try_from(i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    if !(2..=256).contains(&argc) {
        return None;
    }
    let bytes = &bytes[4..];
    let args = &bytes[bytes.iter().position(|b| *b == 0)?..];
    let mut fields = args[args.iter().position(|b| *b != 0)?..].split(|b| *b == 0);
    // Do not interpret argv text or unrelated environment values as identity fields.
    if fields.nth(argc - 1)? != entry.as_os_str().as_bytes() {
        return None;
    }
    let mut home = None;
    let mut marker = None;
    for field in fields.take_while(|s| !s.is_empty()) {
        if let Some(value) = field.strip_prefix(b"HERMES_HOME=") {
            if home.replace(value).is_some() {
                return None;
            }
        }
        if let Some(value) = field.strip_prefix(b"HERMES_TUI_ACTIVE_SESSION_FILE=") {
            if marker.replace(value).is_some() {
                return None;
            }
        }
    }
    if home? != profile.as_os_str().as_bytes() {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(marker?)))
}

pub fn resolve(pane: &str) -> Option<(PathBuf, String)> {
    let profile = home().join(".hermes/profiles/work");
    let dir = profile.join("cache/picker-bindings");
    if !cfg!(target_os = "macos") && !dir.is_dir() {
        return None;
    }
    let info = request(&["pane", "process-info", "--pane", pane]).ok()?;
    let processes = &info["process_info"];
    if processes["pane_id"].as_str()? != pane {
        return None;
    }
    let list = processes["foreground_processes"].as_array()?;
    let mut matches = list
        .iter()
        .filter_map(|p| active(&dir.join(p["pid"].as_u64()?.to_string()), pane));
    if let Some(id) = matches.next() {
        return matches.next().is_none().then_some((profile, id));
    }
    #[cfg(target_os = "macos")]
    {
        let entry = home().join(".hermes/hermes-agent/ui-tui/dist/entry.js");
        let mut nodes = list.iter().filter(|p| {
            p["name"] == "node"
                && p["argv"]
                    .as_array()
                    .and_then(|a| a.last())
                    .and_then(Value::as_str)
                    == entry.to_str()
        });
        let node = nodes.next()?;
        if nodes.next().is_some() {
            return None;
        }
        let pid = i32::try_from(node["pid"].as_u64()?).ok()?;
        if pid <= 0 {
            return None;
        }
        let marker = legacy_marker(pid, &entry, &profile)?;
        Some((profile, session_marker(&marker)?))
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::PermissionsExt};

    #[test]
    fn legacy_identity_requires_exact_argv_and_unique_profile_fields() {
        let entry = Path::new("/hermes/entry.js");
        let profile = Path::new("/work");
        let parse = |tail: &[u8]| {
            let mut b = 2i32.to_ne_bytes().to_vec();
            b.extend_from_slice(b"/node\0\0/node\0");
            b.extend_from_slice(tail);
            parse_marker(&b, entry, profile)
        };
        assert_eq!(
            parse(b"/hermes/entry.js\0HERMES_HOME=/work\0HERMES_TUI_ACTIVE_SESSION_FILE=/a b\0"),
            Some(PathBuf::from("/a b"))
        );
        for invalid in [b"/other.js\0HERMES_HOME=/work\0HERMES_TUI_ACTIVE_SESSION_FILE=/a\0".as_slice(), b"/hermes/entry.js\0HERMES_HOME=/other\0HERMES_TUI_ACTIVE_SESSION_FILE=/a\0", b"/hermes/entry.js\0HERMES_HOME=/work\0X=HERMES_TUI_ACTIVE_SESSION_FILE=/a\0", b"/hermes/entry.js\0HERMES_HOME=/work\0HERMES_TUI_ACTIVE_SESSION_FILE=/a\0HERMES_TUI_ACTIVE_SESSION_FILE=/b\0"] {
            assert_eq!(parse(invalid),None);
        }
    }

    #[test]
    fn follows_only_the_live_panes_marker() {
        let temp = std::env::temp_dir().join(format!("binding-{}", std::process::id()));
        std::fs::create_dir(&temp).unwrap();
        let lease = temp.join("pid");
        let marker = temp.join("hermes-tui-active-session-test");
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lease)
            .unwrap();
        write!(f, "{}", serde_json::json!({"pane":"p1","marker":marker})).unwrap();
        let set = |id: &str| {
            std::fs::write(&marker, serde_json::json!({"session_id":id}).to_string()).unwrap();
            std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        set("session-1");
        assert!(active(&lease, "p1").is_none());
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert!(active(&lease, "p2").is_none());
        assert_eq!(active(&lease, "p1").as_deref(), Some("session-1"));
        set("session-2");
        assert_eq!(active(&lease, "p1").as_deref(), Some("session-2"));
        drop(f);
        assert!(active(&lease, "p1").is_none());
        std::fs::remove_dir_all(temp).unwrap();
    }
}
