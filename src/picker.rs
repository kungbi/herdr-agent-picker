use crate::{
    completion as c,
    reminders::{self, Store},
    util::*,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

pub fn title(s: &str) -> String {
    let text = label(s);
    let model = regex::Regex::new(
        r"(?i)^(?:[\w.-]+/)?(?:gpt-[\w.-]+|claude-[\w.-]+|gemini-[\w.-]+|o\d[\w.-]*)$",
    )
    .unwrap();
    text.split(" · ")
        .enumerate()
        .filter(|(i, p)| *i == 0 || !model.is_match(p))
        .map(|(_, p)| p)
        .collect::<Vec<_>>()
        .join(" · ")
}
fn status_label(status: &str) -> String {
    let (rgb, icon) = match status {
        "working" => ("241;250;140", "●"),
        "blocked" => ("255;85;85", "●"),
        "done" => ("139;233;253", "●"),
        "idle" => ("80;250;123", "○"),
        _ => ("98;114;164", "·"),
    };
    format!("\x1b[38;2;{rgb}m{icon} [{}]\x1b[39m", label(status))
}
pub fn sections(agents: Vec<Value>, times: &Value, now: f64) -> Vec<(String, Vec<Value>)> {
    let (mut done, mut active, mut other, mut dates) = (
        vec![],
        vec![],
        vec![],
        BTreeMap::<String, Vec<Value>>::new(),
    );
    for mut a in agents {
        a["agent_status"] = json!(c::display_status(&a, times));
        match string(&a, "agent_status") {
            "done" => done.push(a),
            "blocked" | "working" => active.push(a),
            _ => {
                if let Some(t) = c::timestamp(&a, times) {
                    dates.entry(date(t)).or_default().push(a)
                } else {
                    other.push(a)
                }
            }
        }
    }
    let sort = |items: &mut Vec<Value>| {
        items.sort_by(|a, b| {
            c::timestamp(b, times)
                .partial_cmp(&c::timestamp(a, times))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    };
    let mut result = vec![];
    if !done.is_empty() {
        sort(&mut done);
        result.push(("Done".into(), done));
    }
    if !active.is_empty() {
        result.push(("Active".into(), active));
    }
    let today = date(now);
    let mut tm = local(now);
    tm.tm_mday -= 1;
    tm.tm_isdst = -1;
    let yesterday = date(unsafe { libc::mktime(&mut tm) } as f64);
    for (day, mut items) in dates.into_iter().rev() {
        sort(&mut items);
        let heading = if day == today {
            "Today".into()
        } else if day == yesterday {
            "Yesterday".into()
        } else {
            day
        };
        result.push((heading, items));
    }
    if !other.is_empty() {
        result.push(("Other".into(), other));
    }
    result
}
fn priority() -> bool {
    crate::config::priority(&fs::read_to_string(config()).unwrap_or_default())
}
pub fn render() -> Result<(String, usize)> {
    let result = request(&["api", "snapshot"])?;
    render_snapshot(&result["snapshot"])
}
// Agent `focused` flags and the initial fzf row are not the session focus.
// A plugin popup has its own HERDR_PANE_ID; only invocation context can
// identify the pane behind it when the session focus points at the popup.
fn preview_origin(snapshot: &Value, context: &Value, popup: Option<&str>) -> Option<String> {
    let candidates = [
        snapshot["focused_pane_id"].as_str(),
        context["focused_pane_id"].as_str(),
    ];
    let mut origin = None;
    for pane in candidates.into_iter().flatten().filter(|p| !p.is_empty()) {
        if Some(pane) == popup {
            continue;
        }
        let exists = ["panes", "agents"].iter().any(|key| {
            snapshot[key]
                .as_array()
                .is_some_and(|panes| panes.iter().any(|p| p["pane_id"].as_str() == Some(pane)))
        });
        if !exists || origin.as_deref().is_some_and(|id| id != pane) {
            return None;
        }
        origin = Some(pane.to_owned());
    }
    origin
}
fn render_snapshot(snapshot: &Value) -> Result<(String, usize)> {
    let mut agents: Vec<Value> = snapshot["agents"]
        .as_array()
        .ok_or("Missing agents")?
        .iter()
        .filter(|a| a["agent"].as_str().is_some_and(|s| !s.is_empty()))
        .cloned()
        .collect();
    if agents.is_empty() {
        return Ok(("@section\t── No running agents ──\n".into(), 1));
    }
    if priority() {
        let rank = |a: &Value| match string(a, "agent_status") {
            "blocked" => 4,
            "working" => 3,
            "done" => 2,
            "idle" => 1,
            _ => 0,
        };
        agents.sort_by_key(|a| {
            std::cmp::Reverse((rank(a), a["state_change_seq"].as_i64().unwrap_or(0)))
        });
    }
    let lookup = |kind: &str, key: &str| -> BTreeMap<String, String> {
        snapshot[kind]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| (string(v, key).into(), string(v, "label").into()))
            .collect()
    };
    let workspaces = lookup("workspaces", "workspace_id");
    let tabs = lookup("tabs", "tab_id");
    let times = c::load(c::cache_path().as_deref());
    let store = Store::new(state_dir());
    let scope = reminders::context()["scope"].as_str().unwrap().to_owned();
    let alarms = store.pending(Some(&scope)).unwrap_or_default();
    let unread = store.unread(&scope).unwrap_or_default();
    let now = now();
    let (mut number, mut selected) = (0, 2);
    let mut rows = vec![];
    for (heading, items) in sections(agents, &times, now) {
        let h = if matches!(heading.as_str(), "Done" | "Active" | "Other") {
            format!(
                "\x1b[1;38;2;{}m── {heading} ──\x1b[22;39m",
                if heading == "Done" {
                    "139;233;253"
                } else {
                    "189;147;249"
                }
            )
        } else {
            format!("\x1b[2m── {heading} ──\x1b[22m")
        };
        rows.push(format!("@section\t{h}"));
        for a in items {
            number += 1;
            if number == 1 || a["focused"] == true {
                selected = rows.len() + 1;
            }
            let n = if number <= 9 {
                format!("[{number}] ")
            } else {
                "    ".into()
            };
            let pane = string(&a, "pane_id");
            let w = string(&a, "workspace_id");
            let tab = string(&a, "tab_id");
            let location = format!(
                "{}/{}",
                label(workspaces.get(w).map(String::as_str).unwrap_or(w)),
                label(tabs.get(tab).map(String::as_str).unwrap_or(tab))
            );
            let name = label(
                ["name", "display_agent", "agent"]
                    .iter()
                    .map(|k| string(&a, k))
                    .find(|s| !s.is_empty())
                    .unwrap_or(""),
            );
            let t = title(
                ["terminal_title_stripped", "title"]
                    .iter()
                    .map(|k| string(&a, k))
                    .find(|s| !s.is_empty())
                    .unwrap_or(""),
            );
            let t = if t.is_empty() { name.clone() } else { t };
            let agent_label = if name != t {
                format!(" · {name}")
            } else {
                String::new()
            };
            let age = c::age(&a, &times, now);
            let age = if age.is_empty() {
                age
            } else {
                format!("\x1b[2m{age}\x1b[22m ")
            };
            let alarm = alarms
                .iter()
                .find(|r| string(r, "pane_id") == pane)
                .map(|r| {
                    format!(
                        " \x1b[38;2;255;184;108m[alarm {}]\x1b[39m",
                        &date_time(r["due"].as_f64().unwrap())[5..]
                    )
                })
                .unwrap_or_default();
            let badge = if unread.contains(pane) {
                "\x1b[38;2;255;85;85m[!]\x1b[39m "
            } else {
                ""
            };
            rows.push(format!("{pane}\t{n}{badge}{} {age}\x1b[1m{t}\x1b[22m \x1b[2m· {location}{agent_label}\x1b[22m{alarm}",status_label(string(&a,"agent_status"))));
        }
    }
    Ok((rows.join("\n") + "\n", selected))
}
pub fn refresh_rows(path: &Path) -> Result<String> {
    match render() {
        Ok((content, _)) => {
            atomic_write(path, content.as_bytes())?;
            Ok(content)
        }
        Err(_) => Ok(fs::read_to_string(path)?),
    }
}
pub fn refresh_action(path: &Path) -> Result<String> {
    let Some(_lock) = lock(&path.with_extension("lock"), true)? else {
        return Ok(String::new());
    };
    let previous = fs::read_to_string(path)?;
    if refresh_rows(path)? == previous {
        return Ok(String::new());
    }
    Ok(format!(
        "reload-sync:cat {}",
        quote(&path.to_string_lossy())
    ))
}
pub fn main() -> Result<()> {
    let fzf = which("fzf").ok_or("fzf is required; see README.md")?;
    let result = request(&["api", "snapshot"])?;
    let context = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let popup = (std::env::var("HERDR_PLUGIN_ID").as_deref() == Ok("agent-picker")
        && std::env::var("HERDR_PLUGIN_ENTRYPOINT_ID").as_deref() == Ok("picker"))
    .then(|| std::env::var("HERDR_PANE_ID").ok())
    .flatten();
    let origin = preview_origin(&result["snapshot"], &context, popup.as_deref());
    let (content, selected) = render_snapshot(&result["snapshot"])?;
    let _ = reminders::ensure_worker(&Store::new(state_dir()));
    let tmp = TempDir::new()?;
    let cache = tmp.0.join("rows");
    fs::write(&cache, &content)?;
    let exe = executable()?;
    let reload = shell(&[&exe, "--refresh", &cache.to_string_lossy()]);
    let reminder = shell(&[&exe]);
    let preview = format!(
        "{} {{1}} {}",
        shell(&[
            &exe,
            "--live-preview",
            &cache.with_extension("preview-lock").to_string_lossy()
        ]),
        quote(origin.as_deref().unwrap_or(""))
    );
    let search = "j,k,a,r,p,q,/,1,2,3,4,5,6,7,8,9,ctrl-d,ctrl-u";
    let mut args=vec!["--ansi".into(),
        "--no-tty-default".into(),"--color=dark,fg+:-1".into(),"--sync".into(),"--no-input".into(),"--no-sort".into(),"--no-multi".into(),"--layout=reverse".into(),"--track".into(),"--id-nth=1".into(),"--raw".into(),"--query=!^──".into(),"--with-shell=/bin/sh -c".into(),"--delimiter=\t".into(),"--with-nth=2..".into(),"--border=rounded".into(),"--border-label= Agents ".into(),"--header=1-9/Enter: focus  Ctrl+D/U: jump  /: search  p: preview  a/r: alarm  Esc: back/close  q: close".into(),format!("--preview={preview}"),"--preview-window=right:60%:wrap-word:follow".into(),format!("--bind=/:show-input+clear-query+disable-raw+unbind({search})"),"--bind=p:toggle-preview,q:abort,resize:refresh-preview".into(),format!("--bind=j:down-match,k:up-match,down:down-match,up:up-match,ctrl-j:down-match,ctrl-k:up-match,load:pos({selected})+unbind(load)"),format!("--bind=esc:transform:if [ \"$FZF_INPUT_STATE\" = hidden ]; then printf abort; else printf '%s' 'change-query(!^──)+enable-raw+rebind({search})'; fi"),"--bind=esc:+hide-input".into(),"--bind=enter:transform:if [ -n {1} ] && [ {1} != '@section' ]; then printf accept; fi".into(),"--bind=double-click:transform:if [ -n {1} ] && [ {1} != '@section' ]; then printf accept; fi".into(),format!("--bind=every(1):bg-transform:{reload}"),format!("--bind=a:execute-silent:{reminder} menu {{1}}"),format!("--bind=r:execute-silent:{reminder} list"),"--bind=a:+clear-screen,r:+clear-screen".into()];
    for n in 1..=9 {
        args.push(format!("--bind={n}:transform:if [ \"$FZF_MATCH_COUNT\" -ge {n} ]; then printf '%s' 'pos(1){}+accept'; fi","+down-match".repeat(n)));
    }
    for (key, dir) in [("ctrl-d", "down"), ("ctrl-u", "up")] {
        args.push(format!(
            "--bind={key}:{}",
            vec![format!("{dir}-match"); 5].join("+")
        ));
    }
    let mut fzf_command = Command::new(fzf);
    if let Some(tty) = tty_path(2) {
        fzf_command.env("HERDR_AGENT_PICKER_TTY_PATH", tty);
    }
    let mut child = fzf_command
        .args(args)
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(content.as_bytes())?;
    let output = child.wait_with_output()?;
    match output.status.code() {
        Some(0) => (),
        Some(1 | 130) => return Ok(()),
        _ => return Err(format!("fzf exited with status {}", output.status).into()),
    };
    let text = String::from_utf8(output.stdout)?;
    let (pane, _) = text.split_once('\t').ok_or("Invalid agent selection")?;
    if pane.is_empty() || pane == "@section" {
        return Err("Invalid agent selection".into());
    }
    request(&["agent", "focus", pane])?;
    let store = Store::new(state_dir());
    let ctx = reminders::context();
    let acknowledge = || -> Result<()> {
        if store.acknowledge(string(&ctx, "scope"), pane)? {
            reminders::sync_sidebar(&store, &ctx, pane)?;
        }
        Ok(())
    };
    if let Err(e) = acknowledge() {
        eprintln!("Could not acknowledge alarm: {e}");
    }
    c::record_event(
        &json!({"event":"pane.focused","data":{"pane_id":pane}}),
        c::cache_path().as_deref(),
        now(),
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_origin_uses_session_focus_or_plugin_context_without_guessing() {
        let mut snapshot = json!({
            "focused_pane_id": "p2",
            "agents": [{"pane_id": "p1", "focused": true}, {"pane_id": "p2", "focused": false}],
            "panes": [{"pane_id": "popup"}, {"pane_id": "shell"}]
        });
        assert_eq!(
            preview_origin(&snapshot, &Value::Null, None).as_deref(),
            Some("p2")
        );
        let context = json!({"focused_pane_id": "p2"});
        snapshot["focused_pane_id"] = json!("popup");
        assert_eq!(
            preview_origin(&snapshot, &context, Some("popup")).as_deref(),
            Some("p2")
        );
        assert_eq!(preview_origin(&snapshot, &Value::Null, Some("popup")), None);
        snapshot["focused_pane_id"] = json!("p1");
        assert_eq!(preview_origin(&snapshot, &context, None), None);
        snapshot["focused_pane_id"] = Value::Null;
        assert_eq!(
            preview_origin(&snapshot, &context, None).as_deref(),
            Some("p2")
        );
        assert_eq!(preview_origin(&snapshot, &Value::Null, None), None);
        assert_eq!(
            preview_origin(&snapshot, &json!({"focused_pane_id": "stale"}), None),
            None
        );
        snapshot["focused_pane_id"] = json!("shell");
        assert_eq!(
            preview_origin(&snapshot, &Value::Null, None).as_deref(),
            Some("shell")
        );
    }
    #[test]
    fn dated_history_is_local_and_sorted_below_done_and_active() {
        let t = |d, h, m| calendar([2026, 10, d, h, m], true).unwrap();
        let agents = vec![
            json!({"pane_id":"blocked","agent_status":"blocked"}),
            json!({"pane_id":"working","agent_status":"working"}),
            json!({"pane_id":"today_old","agent_status":"done"}),
            json!({"pane_id":"yesterday","agent_status":"idle"}),
            json!({"pane_id":"old","agent_status":"done"}),
            json!({"pane_id":"today_new","agent_status":"idle"}),
            json!({"pane_id":"undated","agent_status":"done"}),
            json!({"pane_id":"unknown","agent_status":"unknown"}),
        ];
        let mut times = json!({});
        for (pane, stamp) in [
            ("today_old", t(22, 0, 1)),
            ("today_new", t(22, 0, 4)),
            ("yesterday", t(21, 23, 59)),
            ("old", t(19, 12, 0)),
        ] {
            times[pane] = json!({"status":"done","completed_at":stamp});
        }
        let result = sections(agents, &times, t(22, 0, 5));
        assert_eq!(
            result.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
            ["Done", "Active", "Today", "Yesterday", "Other"]
        );
        assert_eq!(
            result[0]
                .1
                .iter()
                .map(|a| string(a, "pane_id"))
                .collect::<Vec<_>>(),
            ["today_old", "old", "undated"]
        );
        let history = vec![json!({"pane_id":"yesterday","agent_status":"idle"})];
        assert_eq!(sections(history, &times, t(23, 0, 1))[0].0, "2026-10-21");
    }
    #[test]
    fn metadata() {
        assert_eq!(title("Task gpt-6 · gpt-6.1-sol · ~"), "Task gpt-6 · ~");
        assert_eq!(title("Keep · claude-ish task"), "Keep · claude-ish task");
    }
    #[test]
    fn groups() {
        let a = vec![
            json!({"pane_id":"w","agent_status":"working"}),
            json!({"pane_id":"d","agent_status":"done"}),
            json!({"pane_id":"i","agent_status":"idle"}),
        ];
        let result = sections(
            a,
            &json!({"i":{"status":"idle","pending_done":true,"completed_at":42}}),
            100.,
        );
        assert_eq!(
            result.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["Done", "Active"]
        );
        assert_eq!(result[0].1[0]["pane_id"], "i");
    }
}
