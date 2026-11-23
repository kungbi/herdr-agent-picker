//! Test fixture example: excluded from default cargo install and release builds.
//! Every accepted command is synthetic; unknown commands (including notifications) fail.
use serde_json::{json, Value};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
fn flag(name: &str) -> bool {
    env::var_os(name).is_some()
}
fn file(name: &str) -> String {
    env::var(name).unwrap_or_default()
}
fn log(name: &str, s: &str) {
    let path = file(name);
    if !path.is_empty() {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(format!("{s}\n").as_bytes()).unwrap();
    }
}
fn data() -> Value {
    let p = file("DATA");
    if Path::new(&p).is_file() {
        serde_json::from_slice(&fs::read(p).unwrap()).unwrap_or_else(|_| std::process::exit(1))
    } else {
        Value::Null
    }
}
fn snapshot(mode: &str, d: &Value) -> Value {
    if !d.is_null() {
        return d.clone();
    }
    if mode == "reminder" {
        return json!({"agents":[{"agent":"demo","pane_id":"demo:p1","workspace_id":"demo","tab_id":"demo:t1","agent_status":"working","title":"Fictional demo task","focused":true}],"workspaces":[{"workspace_id":"demo","label":"Demo"}],"tabs":[{"tab_id":"demo:t1","label":"Example"}]});
    }
    let count = env::var("PICKER_AGENT_COUNT")
        .unwrap_or("5".into())
        .parse::<usize>()
        .unwrap();
    let states = ["working", "blocked", "done", "idle", "unknown"];
    let agents:Vec<_>=(1..=count).map(|i|json!({"agent":"claude","agent_status":states.get(i-1).unwrap_or(&"idle"),"workspace_id":"w1","tab_id":"w1:t1","pane_id":format!("w1:p{i}"),"focused":i==1,"title":if i==1&&flag("PICKER_UNREAD_ALARM"){"A very long task title ".repeat(12)}else{format!("Agent {i} · gpt-6.1-sol · ~")}})).collect();
    json!({"agents":agents,"workspaces":[{"workspace_id":"w1","label":"Test workspace"}],"tabs":[{"tab_id":"w1:t1","label":"Test tab"}]})
}
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    // Only fixtures opt into RPC mode.
    if args.is_empty() || env::var_os("FAKE_MODE").is_none() {
        return;
    }
    log("CALLS", &serde_json::to_string(&args).unwrap());
    let mode = env::var("FAKE_MODE").unwrap_or("picker".into());
    let pair = args.iter().take(2).map(String::as_str).collect::<Vec<_>>();
    let d = if mode == "live" && pair.as_slice() != ["api", "snapshot"] {
        Value::Null
    } else {
        data()
    };
    let result = match pair.as_slice() {
        ["api", "snapshot"] => {
            assert_eq!(args.len(), 2);
            if flag("SLOW_REFRESH") {
                let calls = fs::read_to_string(file("CALLS")).unwrap();
                if calls.lines().filter(|l| l.contains("\"snapshot\"")).count() > 1 {
                    fs::write(file("REFRESH_STARTED"), "").unwrap();
                    std::thread::sleep(Duration::from_millis(1500));
                }
            }
            let d = if mode == "live" { data() } else { d };
            let s = snapshot(&mode, &d);
            log(
                "EVENTS",
                &format!(
                    "snapshot {}",
                    s["focused_pane_id"].as_str().unwrap_or("None")
                ),
            );
            json!({"snapshot":s})
        }
        ["agent", "list"] => json!({"agents":snapshot(&mode,&d)["agents"]}),
        ["workspace", "list"] => json!({"workspaces":snapshot(&mode,&d)["workspaces"]}),
        ["tab", "list"] => json!({"tabs":snapshot(&mode,&d)["tabs"]}),
        ["agent", "focus"] if mode != "interactive" => {
            assert_eq!(args.len(), 3);
            fs::write(file("FOCUS"), &args[2]).unwrap();
            json!({})
        }
        ["agent", "get"] => {
            if d["get_error"].as_bool() == Some(true) {
                eprintln!("identity lookup unavailable");
                std::process::exit(1);
            }
            if mode == "interactive" {
                json!({"agent":d["agents"].as_array().unwrap().iter().find(|a|a["pane_id"]==args[2]).cloned()})
            } else {
                json!({"agent":{"agent":env::var("PICKER_AGENT_KIND").unwrap_or_else(|_| if mode == "reminder" { "demo".into() } else { "claude".into() })}})
            }
        }
        ["pane", "process-info"] if mode == "interactive" => {
            assert_eq!(args.len(), 4);
            assert_eq!(args[2], "--pane");
            let pane = &args[3];
            let shell = d["shell"].as_bool() == Some(true);
            let pid = if shell {
                10
            } else {
                d["pid"]
                    .as_i64()
                    .unwrap_or(100 + pane.chars().last().unwrap().to_digit(10).unwrap() as i64)
            };
            json!({"process_info":{"pane_id":pane,"shell_pid":10,"foreground_process_group_id":pid,"foreground_processes":[{"pid":pid,"name":if shell{"zsh"}else{"claude"},"argv":["/bin/claude"]}]}})
        }
        ["pane", "get"] => {
            log("EVENTS", &format!("get {}", args[2]));
            if flag("FAIL_ATTACH_SPAWN") {
                fs::set_permissions(
                    env::current_exe().unwrap(),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            json!({"pane":{"terminal_id": if mode == "interactive" { d["agents"].as_array().unwrap().iter().find(|a| a["pane_id"] == args[2]).map(|a| a["terminal_id"].clone()).unwrap_or(Value::Null) } else { json!(args[2]) }}})
        }
        ["terminal", "attach"] => {
            assert_eq!(args.len(), 3, "no takeover allowed");
            let pane = &args[2];
            let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
            unsafe {
                libc::ioctl(0, libc::TIOCGWINSZ, size.as_mut_ptr());
            }
            let size = unsafe { size.assume_init() };
            log(
                "EVENTS",
                &format!(
                    "attach {pane} {} {} {} {} {}",
                    size.ws_col,
                    size.ws_row,
                    file("FZF_PREVIEW_COLUMNS"),
                    file("FZF_PREVIEW_LINES"),
                    std::process::id()
                ),
            );
            if flag("FAIL_ATTACH") {
                std::process::exit(1);
            }
            if flag("ATTACHED") {
                fs::write(file("ATTACHED"), std::process::id().to_string()).unwrap();
            }
            unsafe {
                libc::signal(libc::SIGTERM, stop as *const () as usize);
                libc::signal(libc::SIGINT, stop as *const () as usize);
            }
            print!("\x1b[2J");
            if mode == "interactive" && d["malicious"] == true {
                print!(
                    "\x1b[1;1HLEFT_CORRUPTED\x1b]52;c;ZmFrZQ==\x07\x1bPignored\x1b\\\x1b[?1049h"
                );
            }
            io::stdout().flush().unwrap();
            if mode == "interactive" {
                let mut term: libc::termios = unsafe { std::mem::zeroed() };
                unsafe {
                    libc::tcgetattr(0, &mut term);
                    libc::cfmakeraw(&mut term);
                    libc::tcsetattr(0, libc::TCSANOW, &term);
                }
                fs::write(Path::new(&file("DATA")).with_file_name("received"), []).unwrap();
            }
            while !STOP.load(Ordering::Relaxed) {
                if mode == "interactive" {
                    if data()["end_attach"] == true {
                        break;
                    }
                    let mut p = libc::pollfd {
                        fd: 0,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    if unsafe { libc::poll(&mut p, 1, 10) } > 0 {
                        let mut bytes = [0; 4096];
                        let n = io::stdin().read(&mut bytes).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        let path = Path::new(&file("DATA")).with_file_name("received");
                        OpenOptions::new()
                            .append(true)
                            .open(path)
                            .unwrap()
                            .write_all(&bytes[..n])
                            .unwrap();
                        print!("\x1b[1;1Hfixture response");
                        io::stdout().flush().unwrap();
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            log("EVENTS", &format!("release {pane}"));
            if flag("RELEASED") {
                fs::write(file("RELEASED"), "").unwrap();
            }
            return;
        }
        ["pane", "read"] => {
            // A direct fixture read may request just --format ansi to compare styles.
            assert!(
                args[3..]
                    == [
                        "--source",
                        "recent-unwrapped",
                        "--lines",
                        "30",
                        "--format",
                        "ansi"
                    ]
                    || args[3..] == ["--format", "ansi"]
            );
            log("EVENTS", &format!("read {}", args[2]));
            if flag("FAIL_READ") || d["read_error"] == true {
                std::process::exit(1);
            }
            if flag("SNAPSHOTS") {
                let map: Value =
                    serde_json::from_slice(&fs::read(file("SNAPSHOTS")).unwrap()).unwrap();
                print!("{}", map[&args[2]].as_str().unwrap());
            } else if flag("CONTENT") {
                print!("{}", fs::read_to_string(file("CONTENT")).unwrap());
            } else if mode == "interactive" {
                let path = Path::new(&file("DATA")).with_file_name("received");
                let received = fs::read(path).unwrap_or_default();
                if d["long_preview"] == true {
                    for i in 0..30 {
                        println!("PREVIOUS_ROW_{i:02}");
                    }
                    println!("LIVE ready");
                }
                if d["long_preview"] != true || !received.is_empty() {
                    println!("LIVE {}", String::from_utf8_lossy(&received));
                }
                if d["malicious"] == true {
                    print!("\x1b[1;1HLEFT_CORRUPTED\x1b]52;c;ZmFrZQ==\x07\x1bPignored\x1b\\\x1b[?1049h");
                }
            } else if mode == "resize" {
                println!("\x1b[32mSCREEN_{}\x1b[0m", args[2]);
            } else if mode == "runtime" {
                println!("SNAPSHOT");
            } else {
                let mut text = format!("PREVIEW for {}", args[2]);
                if flag("PICKER_LONG_PREVIEW") {
                    text += &format!("\nLONG_PREVIEW {}END_OF_PREVIEW", "word ".repeat(40));
                }
                if flag("PICKER_PADDED_PREVIEW") {
                    for n in 0..8 {
                        let line = if n == 3 {
                            String::new()
                        } else {
                            format!("    ROW {n}")
                        };
                        text += &format!(
                            "\n{line:289}\x1b[0m\x1b[38;2;22;142;142m{}\x1b[0m",
                            if n < 6 { "┃" } else { "│" }
                        );
                    }
                    text += "\n    한글 들여쓰기   \x1b[0m\nliteral │\nliteral ┃\nPADDED_END";
                }
                if mode == "picker" {
                    println!("\x1b[38;2;17;203;91m{text}\x1b[0m");
                } else {
                    println!("{text}");
                }
            }
            return;
        }
        ["pane", "report-metadata"] if mode != "interactive" => json!({}),
        ["plugin", "pane"] if mode == "runtime" => {
            assert_eq!(
                args,
                [
                    "plugin",
                    "pane",
                    "open",
                    "--plugin",
                    "agent-picker",
                    "--entrypoint",
                    "picker"
                ]
            );
            json!({})
        }
        _ if args == ["--version"] => {
            println!("herdr 0.9.5");
            return;
        }
        _ => {
            log("EVENTS", &format!("unexpected {}", args.join(" ")));
            eprintln!("FORBIDDEN fake Herdr command: {args:?}");
            std::process::exit(2);
        }
    };
    println!("{}", json!({"result":result}));
}
