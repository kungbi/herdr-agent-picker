use crate::{
    completion as c,
    reminders::{self, Store},
    util::*,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

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
fn history_prefix(candidates: &[String], rows: &[String]) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    // A complete frozen window can be replayed verbatim. Match from the tail
    // so repeated lines retain their multiplicity; exclude newer trailing rows.
    if candidates.ends_with(rows) {
        return Some(candidates.len() - rows.len());
    }
    let mut best = (0, 0);
    let mut ambiguous = false;
    for (i, row) in candidates.iter().enumerate().rev() {
        if rows.first() != Some(row) {
            continue;
        }
        let n = candidates[i..]
            .iter()
            .zip(rows)
            .take_while(|(a, b)| a == b)
            .count();
        if n == rows.len() {
            return Some(i);
        }
        if n > best.1 {
            best = (i, n);
            ambiguous = false;
        } else if n == best.1 {
            ambiguous = true;
        }
    }
    (!ambiguous && best.1 >= rows.len().min(2)).then_some(best.0)
}
fn history_anchor(
    candidates: &[String],
    rows: &[String],
    top: usize,
    height: usize,
) -> Option<(usize, usize)> {
    let styles = regex::Regex::new(r"\x1b\[[0-9;:]*m").unwrap();
    let plain = |rows: &[String]| {
        rows.iter()
            .map(|row| {
                let plain = styles.replace_all(row, "");
                let trimmed = plain.trim_start();
                trimmed
                    .strip_prefix('║')
                    .or_else(|| trimmed.strip_prefix("C:\\>"))
                    .map(|s| s.trim_start().to_owned())
                    .unwrap_or_else(|| plain.into_owned())
            })
            .collect::<Vec<_>>()
    };
    let candidates = plain(candidates);
    let rows = plain(rows);
    let mut best = None;
    for start in top..top.saturating_add(height).min(rows.len()) {
        if let Some(index) = history_prefix(&candidates, &rows[start..]) {
            let matched = candidates[index..]
                .iter()
                .zip(&rows[start..])
                .take_while(|(a, b)| a == b)
                .count();
            // shortcut: single-row overlaps cannot be paged safely; use stable row IDs to support them.
            if matched >= 2 && best.is_none_or(|(_, _, length)| matched > length) {
                best = Some((index, start, matched));
            }
        }
    }
    best.map(|(index, start, _)| (index, start))
}
// Reflow may change line breaks, but the seam must be unique and start on a source line.
fn reflow_anchor(
    source: &str,
    rows: &[String],
    top: usize,
    height: usize,
    width: usize,
) -> Option<(usize, usize)> {
    let mut text = String::new();
    let mut line_starts = Vec::new();
    let mut visual = 0;
    let plain_source = crate::preview::public_text(source);
    for line in plain_source.lines() {
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            line_starts.push((text.len(), visual));
            text.push_str(&normalized);
        }
        visual += if line.is_empty() {
            1
        } else {
            crate::preview::visual_rows(line, width.max(1)).len()
        };
    }
    let mut best: Option<(usize, usize, usize)> = None;
    let mut tied = false;
    let normalized_rows: Vec<String> = rows
        .iter()
        .map(|row| {
            crate::preview::public_text(row)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    for start in top..top.saturating_add(height).min(rows.len()) {
        let mut probe = String::new();
        for (offset, normalized) in normalized_rows
            [start..top.saturating_add(height).min(rows.len())]
            .iter()
            .enumerate()
        {
            if normalized.is_empty() {
                break;
            }
            if !probe.is_empty() {
                probe.push(' ');
            }
            probe.push_str(normalized);
            if offset == 0 {
                continue;
            }
            let boundary = |pos: usize| {
                (pos == 0 || text.as_bytes()[pos - 1] == b' ')
                    && (pos + probe.len() == text.len()
                        || text.as_bytes()[pos + probe.len()] == b' ')
            };
            let mut positions = Vec::new();
            let mut search_at = 0;
            while let Some(found) = text[search_at..].find(&probe) {
                let position = search_at + found;
                if boundary(position) {
                    positions.push(position);
                    if positions.len() == 2 {
                        break;
                    }
                }
                search_at = position + probe.chars().next().unwrap().len_utf8();
            }
            let [position] = positions.as_slice() else {
                if positions.is_empty() {
                    break;
                }
                continue;
            };
            let Some((_, prefix)) = line_starts.iter().find(|(pos, _)| pos == position) else {
                continue;
            };
            let length = offset + 1;
            if best.is_none_or(|(_, _, n)| length > n) {
                best = Some((*prefix, start, length));
                tied = false;
            } else if best.is_some_and(|(p, s, n)| length == n && (p, s) != (*prefix, start)) {
                tied = true;
            }
        }
    }
    (!tied)
        .then_some(best?)
        .map(|(prefix, start, _)| (prefix, start))
}
fn cached_source<'a>(sources: &'a [Value], pane: &str, terminal: &str) -> Option<&'a str> {
    sources
        .iter()
        .find(|source| {
            source["pane"].as_str() == Some(pane)
                && source["terminal"].as_str() == Some(terminal)
                && (source["binding"].is_null()
                    || crate::hermes_history::valid(pane, &source["binding"]))
        })?
        .get("text")?
        .as_str()
}
fn history_progress(anchor: Option<(usize, usize)>) -> bool {
    anchor.is_some_and(|(prefix, start)| prefix > start)
}

fn queue_frame(
    path: &Path,
    window: &Value,
    rows: &[String],
    start: usize,
    height: usize,
) -> Result<()> {
    let visible = &rows[start..(start + height).min(rows.len())];
    atomic_write(
        &path.with_extension("history.frame"),
        &serde_json::to_vec(&(
            &window["pane"],
            &window["terminal"],
            &window["semantic"],
            visible.join("\n"),
        ))?,
    )
}

pub fn scroll(path: &Path, step: i32, pane: &str) -> Result<()> {
    let _guard = lock(&path.with_extension("viewport-lock"), false)?;
    let extent = path.with_extension("extent");
    let Ok(extent) = fs::read_to_string(extent) else {
        return Ok(());
    };
    let (rows, height) = extent.split_once(' ').ok_or("Invalid preview extent")?;
    let height: usize = height.parse()?;
    let max = rows.parse::<usize>()?.saturating_sub(height);
    let history = path.with_extension("history");
    let old = match fs::read_to_string(&history) {
        Ok(value) => value.parse::<usize>()?.min(max),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.into()),
    };
    let size = match step.unsigned_abs() {
        1 => 1,
        2 => 2,
        3 => (height / 2).max(1),
        _ => return Err("Invalid scroll step".into()),
    };
    if step < 0 && old.saturating_add(size) >= max {
        let window_path = path.with_extension("window");
        let mut window: Value = serde_json::from_slice(&fs::read(&window_path)?)?;
        if window["pane"].as_str() != Some(pane) {
            return Ok(());
        }
        if !window["semantic"].is_null() && !crate::hermes_history::valid(pane, &window["semantic"])
        {
            return Ok(());
        }
        if window["exhausted"] != true {
            let snapshot_binding = crate::hermes_history::binding(pane);
            let rows: Vec<String> = serde_json::from_value(window["rows"].clone())?;
            let width = window["width"].as_u64().ok_or("Invalid preview width")? as usize;
            let limit = window["limit"].as_u64().ok_or("Invalid preview limit")? as u16;
            // shortcut: reads stop at 65,535 lines, widen the limit type for deeper history.
            let next_limit = limit.saturating_mul(2).max(2048);
            let top = max - old;
            if let Some(terminal) = window["terminal"].as_str() {
                let current = request(&["pane", "get", pane])?;
                if current["pane"]["terminal_id"].as_str() != Some(terminal) {
                    return Ok(());
                }
            }
            let cached = window["terminal"].as_str().and_then(|terminal| {
                let data = fs::read(path.with_extension("sources")).ok()?;
                let sources: Vec<Value> = serde_json::from_slice(&data).ok()?;
                cached_source(&sources, pane, terminal).map(str::to_owned)
            });
            let cached_rows = cached
                .as_ref()
                .map(|text| crate::preview::visual_rows(text, width.max(1)));
            let cached_anchor = cached_rows
                .as_ref()
                .and_then(|candidates| history_anchor(candidates, &rows, top, height))
                .or_else(|| {
                    cached
                        .as_deref()
                        .and_then(|text| reflow_anchor(text, &rows, top, height, width))
                })
                .filter(|anchor| history_progress(Some(*anchor)));
            let (candidates, anchor, short) = if let Some(anchor) = cached_anchor {
                (cached_rows.unwrap(), Some(anchor), false)
            } else {
                let styled = crate::preview::read_history(pane, next_limit).unwrap_or_default();
                let candidates = crate::preview::visual_rows(&styled, width.max(1));
                let anchor = history_anchor(&candidates, &rows, top, height);
                (
                    candidates,
                    anchor,
                    styled.lines().count() < usize::from(next_limit),
                )
            };
            let mut from_transcript = false;
            let (candidates, anchor) = if !history_progress(anchor) && snapshot_binding.is_some() {
                match crate::hermes_history::read(pane, width) {
                    Some((owner, older)) if Some(&owner) == snapshot_binding.as_ref() => {
                        let seam = history_anchor(&older, &rows, top, height).or_else(|| {
                            // Hermes indents rendered answer lines, but not the saved message.
                            let body = rows
                                .iter()
                                .map(|s| s.strip_prefix("    ").unwrap_or(s).to_owned())
                                .collect::<Vec<_>>();
                            history_anchor(&older, &body, top, height)
                        });
                        from_transcript = true;
                        (older, seam)
                    }
                    _ => (candidates, anchor),
                }
            } else {
                (candidates, anchor)
            };
            window["limit"] = json!(next_limit);
            window["exhausted"] = json!(
                anchor.is_some()
                    && !history_progress(anchor)
                    && window["terminal"].is_null()
                    && (short || next_limit == u16::MAX)
            );
            if let Some((prefix, start)) = anchor.filter(|anchor| history_progress(Some(*anchor))) {
                if crate::hermes_history::binding(pane) != snapshot_binding {
                    return Ok(());
                }
                let (prefix, start) = if from_transcript {
                    (prefix - start, 0)
                } else {
                    (prefix, start)
                };
                let mut expanded = candidates[..prefix].to_vec();
                // The retained window may have different styles than this snapshot.
                expanded.last_mut().unwrap().push_str("\x1b[0m");
                expanded.extend_from_slice(&rows[start..]);
                window["semantic"] = json!(snapshot_binding);
                window["rows"] = json!(expanded);
                if from_transcript {
                    window["exhausted"] = json!(true);
                }
                atomic_write(&window_path, serde_json::to_string(&window)?.as_bytes())?;
                atomic_write(&path.with_extension("history.expanded"), b"")?;
                let new_max = expanded.len().saturating_sub(height);
                let target = prefix
                    .saturating_add(top)
                    .saturating_sub(start)
                    .saturating_sub(size);
                atomic_write(
                    &history,
                    new_max.saturating_sub(target).to_string().as_bytes(),
                )?;
                atomic_write(
                    &path.with_extension("extent"),
                    format!("{} {}", expanded.len(), height).as_bytes(),
                )?;
                queue_frame(path, &window, &expanded, target.min(new_max), height)?;
                return Ok(());
            }
            atomic_write(&window_path, serde_json::to_string(&window)?.as_bytes())?;
        }
    }
    let next = if step < 0 {
        old.saturating_add(size).min(max)
    } else {
        old.saturating_sub(size)
    };
    if old == next && !(step > 0 && old == 0 && history.exists()) {
        return Ok(());
    }
    if next == 0 {
        fs::remove_file(history)?;
        let expanded = fs::remove_file(path.with_extension("history.expanded")).is_ok();
        if fs::remove_file(path.with_extension("history.resized")).is_ok() || expanded {
            print!("change-preview-window(default:right:60%:nowrap:follow:+0)+refresh-preview+");
        }
        println!("preview-bottom");
    } else {
        atomic_write(&history, next.to_string().as_bytes())?;
        if path.with_extension("history.expanded").exists()
            || path.with_extension("history.resized").exists()
        {
            let window: Value = serde_json::from_slice(&fs::read(path.with_extension("window"))?)?;
            if window["pane"].as_str() == Some(pane)
                && (window["semantic"].is_null()
                    || crate::hermes_history::valid(pane, &window["semantic"]))
            {
                let rows: Vec<String> = serde_json::from_value(window["rows"].clone())?;
                queue_frame(
                    path,
                    &window,
                    &rows,
                    max.saturating_sub(next).min(rows.len()),
                    height,
                )?;
            }
            return Ok(());
        }

        let action = if next > old {
            "preview-up"
        } else {
            "preview-down"
        };
        println!("{}", vec![action; old.abs_diff(next)].join("+"));
    }
    Ok(())
}

pub fn focus_preview(path: &Path, pane: &str) -> Result<()> {
    let _guard = lock(&path.with_extension("viewport-lock"), false)?;
    let history = path.with_extension("history");
    if history.exists()
        && fs::read(path.with_extension("window"))
            .ok()
            .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
            .is_some_and(|window| window["pane"].as_str() == Some(pane))
    {
        return Ok(());
    }
    let expanded = path.with_extension("history.expanded").exists();
    let resized = path.with_extension("history.resized").exists();
    for file in [
        history,
        path.with_extension("history.expanded"),
        path.with_extension("history.resized"),
    ] {
        match fs::remove_file(file) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    if expanded || resized {
        print!("change-preview-window(default:right:60%:nowrap:follow:+0)");
    }
    Ok(())
}

pub fn refresh_action(path: &Path) -> Result<String> {
    let paused =
        || path.with_extension("active").exists() || path.with_extension("history").exists();
    if paused() {
        return Ok(String::new());
    }
    let Some(_lock) = lock(&path.with_extension("lock"), true)? else {
        return Ok(String::new());
    };
    let previous = fs::read_to_string(path)?;
    let (next, _) = render()?;
    if next == previous || paused() {
        return Ok(String::new());
    }
    atomic_write(path, next.as_bytes())?;
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
    let search = "j,k,J,K,ㅓ,ㅏ,U,D,a,r,p,q,/,1,2,3,4,5,6,7,8,9,ctrl-d,ctrl-u";
    // Reloading the agent list restarts fzf's preview, even for the same pane.
    let history = quote(&cache.with_extension("history").to_string_lossy());
    let mut args=vec!["--ansi".into(),
        "--no-tty-default".into(),"--color=dark,fg+:-1".into(),"--sync".into(),"--no-input".into(),"--no-sort".into(),"--no-multi".into(),"--layout=reverse".into(),"--track".into(),"--id-nth=1".into(),"--raw".into(),"--query=!^──".into(),"--with-shell=/bin/sh -c".into(),"--delimiter=\t".into(),"--with-nth=2..".into(),"--border=rounded".into(),"--border-label= Agents ".into(),"--header=1-9/Enter: focus  J/K: down/up 2 lines  U/D: half page  Ctrl+D/U: jump  /: search  p: preview  i: interact  a/r: alarm  Esc: back/close  q: close".into(),format!("--preview={preview}"),"--preview-window=right:60%:nowrap:follow".into(),format!("--bind=/:show-input+clear-query+disable-raw+unbind({search})"),"--bind=p:toggle-preview,q:abort,ctrl-l:clear-screen+change-preview-label( Preview )".into(),format!("--bind=j:down-match,k:up-match,down:down-match,up:up-match,ctrl-j:down-match,ctrl-k:up-match,load:pos({selected})+unbind(load)"),format!("--bind=esc:ignore,ctrl-]:transform:if [ \"$FZF_INPUT_STATE\" = hidden ]; then printf abort; else printf '%s' 'change-query(!^──)+enable-raw+rebind({search})'; fi"),"--bind=ctrl-]:+hide-input".into(),"--bind=enter:transform:if [ -n {1} ] && [ {1} != '@section' ]; then printf accept; fi".into(),"--bind=double-click:transform:if [ -n {1} ] && [ {1} != '@section' ]; then printf accept; fi".into(),format!("--bind=every(1):bg-transform:{reload}"),format!("--bind=a:execute-silent:{reminder} menu {{1}}"),format!("--bind=r:execute-silent:{reminder} list"),"--bind=a:+clear-screen,r:+clear-screen".into()];
    args.push(format!(
        "--bind=resize:transform:if [ -f {history} ]; then touch {history}.resized; fi; printf refresh-preview"
    ));
    let scroll = shell(&[&exe, "--scroll", &cache.to_string_lossy()]);
    for (keys, step) in [
        ("K,ㅏ", -2),
        ("J,ㅓ", 2),
        ("shift-up,preview-scroll-up", -1),
        ("shift-down,preview-scroll-down", 1),
        ("U", -3),
        ("D", 3),
    ] {
        args.push(format!("--bind={keys}:transform:{scroll} {step} {{1}}"));
    }
    args.push(format!(
        "--bind=end:transform:rm -f {history}; if [ -f {history}.expanded ]; then rm -f {history}.expanded {history}.resized; printf '%s' 'change-preview-window(default:right:60%:nowrap:follow:+0)+refresh-preview'; elif [ -f {history}.resized ]; then rm -f {history}.resized; printf '%s' 'change-preview-window(default:right:60%:nowrap:follow:+0)+refresh-preview+preview-bottom'; else printf preview-bottom; fi"
    ));
    args.push(format!(
        "--bind=focus:transform:{} {{1}}",
        shell(&[&exe, "--focus-preview", &cache.to_string_lossy()])
    ));
    for n in 1..=9 {
        args.push(format!("--bind={n}:transform:if [ \"$FZF_MATCH_COUNT\" -ge {n} ]; then printf '%s' 'pos(1){}+accept'; fi","+down-match".repeat(n)));
    }
    args.push(format!(
        "--bind=i:transform:{} {{1}} {{q}}",
        shell(&[
            &exe,
            "--interact",
            &cache.with_extension("preview-lock").to_string_lossy()
        ])
    ));
    for (key, dir) in [("ctrl-d", "down"), ("ctrl-u", "up")] {
        args.push(format!(
            "--bind={key}:{}",
            vec![format!("{dir}-match"); 5].join("+")
        ));
    }
    let mut fzf_command = Command::new(fzf);
    fzf_command
        .args(args)
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE");
    let output = crate::interaction::run(
        fzf_command,
        content.as_bytes(),
        &cache.with_extension("preview-lock"),
    )?;
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
    fn short_history_does_not_exhaust_while_the_window_grows() {
        assert!(history_progress(Some((74, 0))));
        assert!(history_progress(Some((2, 1))));
        assert!(!history_progress(Some((1, 1))));
        assert!(!history_progress(None));
    }
    #[test]
    fn cached_history_preserves_original_colors() {
        let text = "old red\nold green\nnew\n";
        let ansi = "\x1b[31mold red\x1b[0m\n\x1b[32mold green\x1b[0m\nnew\n";
        let sources = vec![
            json!({"pane":"p1", "terminal":"t1", "text":crate::preview::styled_history(text, ansi)}),
        ];
        let rows = crate::preview::visual_rows(cached_source(&sources, "p1", "t1").unwrap(), 80);
        assert!(rows[0].contains("\x1b[31mold red"));
        assert!(rows[1].contains("\x1b[32mold green"));
    }
    #[test]
    fn attached_history_uses_only_matching_pre_attach_source() {
        let text: String = (1..=120).map(|n| format!("CX{n:03}\n")).collect();
        let visible: Vec<_> = (94..=120)
            .map(|n| format!("\x1b[32mCX{n:03}\x1b[0m"))
            .collect();
        let sources = vec![
            json!({"pane":"p2", "terminal":"t1", "text":"wrong pane\n"}),
            json!({"pane":"p1", "terminal":"old", "text":"wrong terminal\n"}),
            json!({"pane":"p1", "terminal":"t1", "text":text}),
        ];
        assert_eq!(cached_source(&sources, "p1", "other"), None);
        assert_eq!(cached_source(&sources, "other", "t1"), None);
        let candidates =
            crate::preview::visual_rows(cached_source(&sources, "p1", "t1").unwrap(), 50);
        let (prefix, start) = history_anchor(&candidates, &visible, 0, 10).unwrap();
        let mut rows = candidates[..prefix].to_vec();
        rows.extend_from_slice(&visible[start..]);
        assert_eq!(rows.len(), 120);
        assert_eq!(rows[0], "CX001");
        assert_eq!(rows[92], "CX093");
        assert_eq!(rows[93], "\x1b[32mCX094\x1b[0m");
    }
    #[test]
    fn attached_word_wrapped_history_finds_exact_multirow_seam() {
        let source: String = (1..=120).map(|n| format!("    • HX{n:03} item: Verify that the timer still advances after opening the view for this entry.\n")).collect();
        let native: Vec<String> = (105..=120).flat_map(|n| [
            format!("    • \x1b[1mHX{n:03}\x1b[0m item: Verify that the timer still advances after"),
            "      opening the view for this entry.".to_string(),
        ]).collect();
        let candidates = crate::preview::visual_rows(&source, 68);
        let (prefix, start) =
            reflow_anchor(&source, &native, 0, 32, 68).expect("exact reflow seam");
        assert_eq!(start, 0);
        assert!(candidates[prefix].contains("HX105"));
        let mut merged = candidates[..prefix].to_vec();
        merged.extend_from_slice(&native[start..]);
        assert!(merged.ends_with(&native));
        assert!(merged[..prefix].iter().any(|r| r.contains("HX104")));
        assert_eq!(
            reflow_anchor(&(source.clone() + &source), &native, 0, 32, 68),
            None
        );
        let mismatch = vec![
            "    • HX105 item: Verify that the timer still advances after".to_string(),
            "      opening the view for a different entry.".to_string(),
        ];
        assert_eq!(reflow_anchor(&source, &mismatch, 0, 2, 68), None);
        assert_eq!(reflow_anchor(&source, &native[..1], 0, 1, 68), None);
        let repeated_suffix = vec![
            "    opening the view for this entry.".to_owned(),
            "    opening the view for this entry.".to_owned(),
        ];
        assert_eq!(reflow_anchor(&source, &repeated_suffix, 0, 2, 68), None);
        assert_eq!(
            reflow_anchor("A A A", &["A".into(), "A".into()], 0, 2, 68),
            None
        );
        let mut continuation_first = vec!["    opening the view for this entry.".to_owned()];
        continuation_first.extend(native[4..].iter().cloned());
        let (next_prefix, next_start) =
            reflow_anchor(&source, &continuation_first, 0, 29, 68).unwrap();
        assert_eq!(next_start, 1);
        assert!(candidates[next_prefix].contains("HX107"));
        let source_with_blanks = format!("prior\n\n{source}");
        let candidates_with_blanks = crate::preview::visual_rows(&source_with_blanks, 68);
        let (prefix_with_blanks, _) =
            reflow_anchor(&source_with_blanks, &native, 0, 32, 68).unwrap();
        assert!(candidates_with_blanks[prefix_with_blanks].contains("HX105"));
    }
    #[test]
    fn history_anchor_accepts_full_replays_but_rejects_ambiguous_fragments() {
        let rows = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            history_prefix(&rows(&["old", "A", "B", "new"]), &rows(&["A", "B", "tail"])),
            Some(1)
        );
        assert_eq!(
            history_prefix(&rows(&["A", "B", "A", "B"]), &rows(&["A", "B"])),
            Some(2)
        );
        assert_eq!(
            history_prefix(
                &rows(&["old", "A", "B", "A", "B", "new"]),
                &rows(&["A", "B"])
            ),
            Some(3)
        );
        assert_eq!(
            history_prefix(
                &rows(&["A", "B", "x", "A", "B", "y"]),
                &rows(&["A", "B", "tail"])
            ),
            None
        );
        assert_eq!(
            history_prefix(&rows(&["old", "A", "other"]), &rows(&["A", "B"])),
            None
        );
    }
    #[test]
    fn history_anchor_normalizes_hermes_first_line() {
        let candidates = vec!["older".into(), "the question".into(), "second line".into()];
        let rows = vec!["║ the question".into(), "second line".into()];
        assert_eq!(history_anchor(&candidates, &rows, 0, 2), Some((1, 0)));
        let rows = vec!["C:\\> the question".into(), "second line".into()];
        assert_eq!(history_anchor(&candidates, &rows, 0, 2), Some((1, 0)));
    }
    #[test]
    fn history_anchor_skips_sticky_header_and_preserves_body_order() {
        let candidates: Vec<_> = (1..=120).map(|n| format!("CX{n:03}")).collect();
        let mut rows = vec!["truncated sticky prompt".to_string()];
        rows.extend((69..=120).map(|n| format!("\x1b[32mCX{n:03}\x1b[0m")));
        rows.push("footer".to_string());
        let (prefix, start) = history_anchor(&candidates, &rows, 0, 12).unwrap();
        assert_eq!((prefix, start), (68, 1));
        let mut expanded = candidates[..prefix].to_vec();
        expanded.extend_from_slice(&rows[start..]);
        let plain = regex::Regex::new(r"\x1b\[[0-9;:]*m").unwrap();
        assert_eq!(
            expanded[..120]
                .iter()
                .map(|row| plain.replace_all(row, "").into_owned())
                .collect::<Vec<_>>(),
            candidates
        );
        assert!(expanded[68].starts_with("\x1b[32m"));
        assert_eq!(prefix.saturating_sub(start).saturating_sub(2), 65);
    }
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
