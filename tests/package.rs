//! The sole Python exception: invoke the real production packaging tool, never fixture code.
mod common;
use common::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
const COMMON:&str="LICENSE README.md CHANGELOG.md herdr-plugin.toml run docs/rust-port-validation.md docs/scenario-parity.md docs/parity-picker.md docs/parity-preview.md docs/parity-dialogs.md docs/images/agent-picker-demo.png docs/images/agent-picker-demo.svg docs/images/agent-picker-preview.png docs/images/agent-picker-alarm.png";
const SOURCE:&str="Cargo.toml Cargo.lock scripts/build_package.py tests/common/mod.rs tests/support/fake_herdr.rs tests/runtime.rs tests/picker.rs tests/live_picker.rs tests/composer.rs tests/conversation_picker.rs tests/preview_resize.rs tests/reminder_picker.rs tests/package.rs src/main.rs src/config.rs src/util.rs src/completion.rs src/picker.rs src/preview.rs src/reminders.rs src/dialog.rs src/composer.rs src/sqlite.rs .github/workflows/preview.yml";
fn checked(c: Command) -> Output {
    let o = output(c, Duration::from_secs(60));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o
}
fn cmd(name: &str, args: &[&OsStr]) -> Command {
    let mut c = Command::new(name);
    c.args(args);
    c
}
use std::ffi::OsStr;
fn python() -> String {
    if let Ok(p) = std::env::var("PYTHON") {
        return p;
    }
    for p in [
        "python3.14",
        "python3.13",
        "python3.12",
        "python3.11",
        "python3",
    ] {
        if Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return p.into();
        }
    }
    panic!("production packaging tool requires Python 3.11+")
}
fn package(f: &Fixture, dir: &str, source: bool, extra: &[&str]) -> Output {
    let mut c = Command::new(python());
    c.arg(Path::new(ROOT).join("scripts/build_package.py"))
        .arg("--output")
        .arg(f.path(dir))
        .arg("--binary")
        .arg(BINARY)
        .args(extra);
    if source {
        c.arg("--source");
    }
    output(c, Duration::from_secs(30))
}
fn oct(b: &[u8]) -> u64 {
    u64::from_str_radix(std::str::from_utf8(b).unwrap().trim_matches(['\0', ' ']), 8).unwrap_or(0)
}
fn le16(b: &[u8], i: usize) -> usize {
    u16::from_le_bytes(b[i..i + 2].try_into().unwrap()) as usize
}
fn le32(b: &[u8], i: usize) -> usize {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize
}
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    bytes: Vec<u8>,
    mode: u32,
}
fn tar(path: &Path) -> BTreeMap<String, Entry> {
    let compressed = fs::read(path).unwrap();
    assert_eq!(&compressed[4..8], &[0, 0, 0, 0], "gzip timestamp");
    let mut c = Command::new("gzip");
    c.arg("-dc").arg(path);
    let bytes = checked(c).stdout;
    let mut entries = BTreeMap::new();
    let mut pos = 0;
    while pos + 512 <= bytes.len() {
        let h = &bytes[pos..pos + 512];
        if h.iter().all(|v| *v == 0) {
            assert!(bytes[pos..].iter().all(|v| *v == 0));
            break;
        }
        let name =
            String::from_utf8(h[..100].iter().copied().take_while(|v| *v != 0).collect()).unwrap();
        assert!(!name.starts_with('/') && !name.split('/').any(|p| p == ".."));
        assert!(matches!(h[156], 0 | b'0'), "non-regular member {name}");
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    *b as u64
                }
            })
            .sum();
        assert_eq!(oct(&h[148..156]), sum);
        assert_eq!(oct(&h[136..148]), 315532800);
        assert_eq!(oct(&h[108..116]), 0);
        assert_eq!(oct(&h[116..124]), 0);
        let n = oct(&h[124..136]) as usize;
        let mode = oct(&h[100..108]) as u32;
        assert!(
            entries
                .insert(
                    name,
                    Entry {
                        bytes: bytes[pos + 512..pos + 512 + n].to_vec(),
                        mode
                    }
                )
                .is_none(),
            "duplicate tar member"
        );
        pos += 512 + n.div_ceil(512) * 512;
    }
    entries
}
fn zip_modes(path: &Path) -> BTreeMap<String, u32> {
    let b = fs::read(path).unwrap();
    let eocd = b.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    let count = le16(&b, eocd + 10);
    let mut pos = le32(&b, eocd + 16);
    let mut modes = BTreeMap::new();
    for _ in 0..count {
        assert_eq!(&b[pos..pos + 4], b"PK\x01\x02");
        assert_eq!(b[pos + 5], 3, "Unix zip creator");
        assert_eq!(le16(&b, pos + 12), 0);
        assert_eq!(le16(&b, pos + 14), 33);
        let n = le16(&b, pos + 28);
        let name = String::from_utf8(b[pos + 46..pos + 46 + n].to_vec()).unwrap();
        let mode = (le32(&b, pos + 38) >> 16) as u32;
        assert_eq!(mode & 0o170000, 0o100000);
        assert!(
            modes.insert(name, mode & 0o777).is_none(),
            "duplicate zip member"
        );
        pos += 46 + n + le16(&b, pos + 30) + le16(&b, pos + 32);
    }
    assert_eq!(pos, le32(&b, eocd + 16) + le32(&b, eocd + 12));
    modes
}
fn source_check(extracted: &Path, f: &Fixture) {
    // Validate the extracted manifest and lockfile too. A separate target dir
    // avoids outer Cargo build locks and any dependence on its artifact layout.
    // `check` compiles without recursively executing this archive test.
    let mut c = Command::new(env!("CARGO"));
    c.current_dir(extracted)
        .args([
            "check",
            "--locked",
            "--offline",
            "--all-targets",
            "--target-dir",
        ])
        .arg(f.path("source-target"));
    checked(c);
}
#[test]
fn deterministic_effective_native_and_source_archives() {
    let f = Fixture::new("package");
    let version = env!("CARGO_PKG_VERSION");
    let tag = format!("v{version}");
    for source in [false, true] {
        let first = package(&f, "first", source, &["--tag", &tag]);
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        let second = package(&f, "second", source, &["--tag", &tag]);
        assert!(second.status.success());
        let names: Vec<PathBuf> = String::from_utf8(first.stdout)
            .unwrap()
            .lines()
            .map(PathBuf::from)
            .collect();
        assert_eq!(names.len(), 3);
        for p in &names {
            assert_eq!(
                fs::read(p).unwrap(),
                fs::read(f.path("second").join(p.file_name().unwrap())).unwrap()
            );
        }
        let archive = tar(&names[0]);
        let top = archive.keys().next().unwrap().split('/').next().unwrap();
        let mut files: BTreeSet<&str> = COMMON.split_whitespace().collect();
        if source {
            files.extend(SOURCE.split_whitespace());
        } else {
            files.insert("bin/herdr-agent-picker");
        }
        let expected: BTreeSet<String> = files.iter().map(|s| format!("{top}/{s}")).collect();
        assert_eq!(archive.keys().cloned().collect::<BTreeSet<_>>(), expected);
        let modes = zip_modes(&names[1]);
        assert_eq!(modes.keys().cloned().collect::<BTreeSet<_>>(), expected);
        checked(cmd("unzip", &[OsStr::new("-tq"), names[1].as_os_str()]));
        let unpacked = f.path(if source {
            "source-unpacked"
        } else {
            "native-unpacked"
        });
        checked(cmd(
            "unzip",
            &[
                OsStr::new("-qq"),
                names[1].as_os_str(),
                OsStr::new("-d"),
                unpacked.as_os_str(),
            ],
        ));
        for (name, entry) in &archive {
            let relative = name.strip_prefix(&format!("{top}/")).unwrap();
            let executable = relative == "run" || relative == "bin/herdr-agent-picker";
            assert_eq!(entry.mode, if executable { 0o755 } else { 0o644 });
            assert_eq!(modes[name], entry.mode);
            assert_eq!(
                fs::read(unpacked.join(name)).unwrap(),
                entry.bytes,
                "ZIP effective payload {name}"
            );
            let original = if relative == "bin/herdr-agent-picker" {
                PathBuf::from(BINARY)
            } else {
                Path::new(ROOT).join(relative)
            };
            assert_eq!(
                entry.bytes,
                fs::read(original).unwrap(),
                "TAR effective payload {name}"
            );
        }
        for line in fs::read_to_string(&names[2]).unwrap().lines() {
            let (sum, name) = line.split_once("  ").unwrap();
            use sha2::{Digest, Sha256};
            assert_eq!(
                sum,
                format!(
                    "{:x}",
                    Sha256::digest(fs::read(names[2].parent().unwrap().join(name)).unwrap())
                )
            );
        }
        let extracted = unpacked.join(top);
        checked(cmd(
            "sh",
            &[OsStr::new("-n"), extracted.join("run").as_os_str()],
        ));
        assert!(!extracted.join("agent-picker.py").exists());
        if source {
            source_check(&extracted, &f);
        } else {
            assert_eq!(
                fs::metadata(extracted.join("bin/herdr-agent-picker"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            let mut c = Command::new("sh");
            c.arg(extracted.join("run"))
                .arg("--version")
                .env("PYTHON", "/nonexistent")
                .env("HERDR_AGENT_PICKER_PYTHON", "/nonexistent");
            let o = checked(c);
            assert!(String::from_utf8_lossy(&o.stdout).starts_with("herdr-agent-picker "));
        }
    }
}
#[test]
fn packaging_rejects_bad_tag_and_script_binary() {
    let f = Fixture::new("bad-package");
    assert!(!package(&f, "bad", false, &["--tag", "v0.0.0-wrong"])
        .status
        .success());
    f.write("script", "#!/bin/sh\nexit 0\n");
    assert!(!package(
        &f,
        "bad",
        false,
        &["--binary", f.path("script").to_str().unwrap()]
    )
    .status
    .success());
}
