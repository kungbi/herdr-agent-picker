use crate::util::*;
use serde_json::{json, Value};
use std::{
    env, fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
pub fn cache_path() -> Option<PathBuf> {
    let socket = env::var("HERDR_SOCKET_PATH")
        .ok()
        .filter(|s| !s.is_empty())?;
    let stat = fs::metadata(&socket).ok()?;
    let ns = stat.ctime() as i128 * 1_000_000_000 + stat.ctime_nsec() as i128;
    let identity = format!("{socket}:{}:{ns}", stat.ino());
    Some(
        state_dir()
            .join("completions")
            .join(format!("{}.json", &hash(&identity)[..24])),
    )
}
pub fn load(path: Option<&Path>) -> Value {
    let Some(path) = path else { return json!({}) };
    let contents = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => config().parent().and_then(|p| {
            fs::read_to_string(p.join("agent-completions").join(path.file_name()?)).ok()
        }),
        Err(_) => None,
    };
    contents
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
pub fn pending(entry: &Value) -> bool {
    entry
        .get("pending_done")
        .map(|v| v == true)
        .unwrap_or_else(|| {
            string(entry, "status") == "idle"
                && !entry["completed_at"].is_null()
                && entry["completed_at"] == entry["observed_at"]
        })
}
pub fn timestamp(agent: &Value, times: &Value) -> Option<f64> {
    if !matches!(string(agent, "agent_status"), "done" | "idle") {
        return None;
    }
    let e = &times[string(agent, "pane_id")];
    if !matches!(string(e, "status"), "done" | "idle") {
        return None;
    }
    e["completed_at"].as_f64().filter(|t| t.is_finite())
}
pub fn display_status<'a>(agent: &'a Value, times: &Value) -> &'a str {
    let status = string(agent, "agent_status");
    if status == "idle"
        && pending(&times[string(agent, "pane_id")])
        && timestamp(agent, times).is_some()
    {
        "done"
    } else {
        status
    }
}
pub fn age(agent: &Value, times: &Value, now: f64) -> String {
    let Some(t) = timestamp(agent, times) else {
        return String::new();
    };
    let elapsed = (now - t).max(0.) as u64;
    if elapsed < 10 {
        return "just now".into();
    }
    for (unit, seconds) in [("d", 86400), ("h", 3600), ("m", 60), ("s", 1)] {
        if elapsed >= seconds {
            return format!("{}{} ago", elapsed / seconds, unit);
        }
    }
    String::new()
}
pub fn record_event(event: &Value, path: Option<&Path>, observed: f64) -> Result<()> {
    let Some(path) = path else { return Ok(()) };
    let pane = string(&event["data"], "pane_id");
    if pane.is_empty() {
        return Ok(());
    }
    private_dir(path.parent().ok_or("Missing cache parent")?)?;
    let _lock = lock(&path.with_extension("lock"), false)?;
    let mut times = load(Some(path));
    let previous = times[pane].clone();
    let previous = if previous.is_object() {
        previous
    } else {
        json!({})
    };
    if observed < previous["observed_at"].as_f64().unwrap_or(0.) {
        return Ok(());
    }
    match string(event, "event").replace('.', "_").as_str() {
        "pane_focused" => {
            if previous.as_object().unwrap().is_empty() {
                return Ok(());
            }
            times[pane] = previous;
            times[pane]["pending_done"] = json!(false);
            times[pane]["observed_at"] = json!(observed);
        }
        "pane_agent_status_changed" => {
            let status = string(&event["data"], "agent_status");
            let old = string(&previous, "status");
            let mut completed = previous["completed_at"].clone();
            let mut pending_done = pending(&previous);
            match status {
                "done" => {
                    pending_done = false;
                    if !matches!(old, "done" | "idle") || completed.is_null() {
                        completed = json!(observed);
                    }
                }
                "idle" => {
                    if matches!(old, "working" | "blocked") {
                        completed = json!(observed);
                        pending_done = true;
                    }
                }
                _ => {
                    completed = Value::Null;
                    pending_done = false;
                }
            }
            times[pane] = json!({"status":status,"completed_at":completed,"observed_at":observed,"pending_done":pending_done});
        }
        _ => {
            times.as_object_mut().unwrap().remove(pane);
        }
    }
    atomic_write(path, &serde_json::to_vec(&times)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transitions_migration_and_stale_events() {
        let dir = TempDir::new().unwrap();
        let path = dir.0.join("times.json");
        let pane = json!({"pane_id":"p","agent_status":"idle"});
        let event = |status: &str| json!({"event":"pane.agent_status_changed","data":{"pane_id":"p","agent_status":status}});
        for (s, t) in [
            ("working", 200.),
            ("idle", 300.),
            ("idle", 310.),
            ("working", 250.),
        ] {
            record_event(&event(s), Some(&path), t).unwrap();
        }
        let times = load(Some(&path));
        assert_eq!(display_status(&pane, &times), "done");
        assert_eq!(age(&pane, &times, 480.), "3m ago");
        record_event(
            &json!({"event":"pane.focused","data":{"pane_id":"p"}}),
            Some(&path),
            320.,
        )
        .unwrap();
        assert_eq!(display_status(&pane, &load(Some(&path))), "idle");
        assert_eq!(timestamp(&pane, &load(Some(&path))), Some(300.));
        fs::write(
            &path,
            r#"{"p":{"status":"idle","completed_at":660,"observed_at":660}}"#,
        )
        .unwrap();
        assert_eq!(display_status(&pane, &load(Some(&path))), "done");
        for e in ["pane.closed", "pane.exited", "pane.agent_detected"] {
            record_event(&event("done"), Some(&path), 800.).unwrap();
            record_event(
                &json!({"event":e,"data":{"pane_id":"p"}}),
                Some(&path),
                900.,
            )
            .unwrap();
            assert_eq!(load(Some(&path)), json!({}));
        }
    }
    #[test]
    fn done_idle_and_new_work() {
        let dir = TempDir::new().unwrap();
        let path = dir.0.join("times.json");
        for (s, t) in [("done", 300.), ("done", 350.), ("idle", 360.)] {
            record_event(&json!({"event":"pane_agent_status_changed","data":{"pane_id":"p","agent_status":s}}),Some(&path),t).unwrap();
        }
        let a = json!({"pane_id":"p","agent_status":"idle"});
        assert_eq!(display_status(&a, &load(Some(&path))), "idle");
        assert_eq!(timestamp(&a, &load(Some(&path))), Some(300.));
        record_event(&json!({"event":"pane_agent_status_changed","data":{"pane_id":"p","agent_status":"working"}}),Some(&path),500.).unwrap();
        assert_eq!(timestamp(&a, &load(Some(&path))), None);
    }
}
