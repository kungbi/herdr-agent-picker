use crate::{dialog::Dialog, sqlite::Database, util::*};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    env,
    fs::{self, OpenOptions},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub fn context() -> Value {
    let socket = env::var("HERDR_SOCKET_PATH").unwrap_or_default();
    let mut identity = socket.clone();
    if !socket.is_empty() {
        if let Ok(s) = fs::metadata(&socket) {
            let ns = s.ctime() as i128 * 1_000_000_000 + s.ctime_nsec() as i128;
            identity.push_str(&format!(":{}:{}:{ns}", s.dev(), s.ino()));
        }
    }
    let bin = which(&herdr())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "herdr".into());
    json!({"scope":hash(&identity),"socket":socket,"config":env::var("HERDR_CONFIG_PATH").unwrap_or_default(),"herdr":bin})
}
pub struct Store {
    pub root: PathBuf,
}
impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn database(&self, create: bool) -> Result<Option<Database>> {
        let path = self.root.join("reminders.sqlite3");
        if !create && !path.exists() {
            return Ok(None);
        }
        private_dir(&self.root)?;
        let db = Database::open(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        if create {
            db.transaction(|db|{db.execute("CREATE TABLE IF NOT EXISTS reminders (id TEXT PRIMARY KEY, scope TEXT NOT NULL, pane_id TEXT NOT NULL, title TEXT NOT NULL, due REAL NOT NULL, context TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', retry_at REAL NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, error TEXT, UNIQUE(scope, pane_id))",&[])?;db.execute("CREATE TABLE IF NOT EXISTS unread_reminders (scope TEXT NOT NULL, pane_id TEXT NOT NULL, PRIMARY KEY (scope, pane_id))",&[])?;Ok(())})?;
        }
        Ok(Some(db))
    }
    pub fn pending(&self, scope: Option<&str>) -> Result<Vec<Value>> {
        let Some(db) = self.database(false)? else {
            return Ok(vec![]);
        };
        match scope{Some(s)=>db.query("SELECT * FROM reminders WHERE status IN ('pending','delivering') AND scope=? ORDER BY due",&[json!(s)]),None=>db.query("SELECT * FROM reminders WHERE status IN ('pending','delivering') ORDER BY due",&[])}
    }
    pub fn schedule(
        &self,
        pane: &str,
        title: &str,
        due: f64,
        ctx: &Value,
        now: f64,
    ) -> Result<String> {
        if !due.is_finite() || due <= now {
            return Err("Choose a time in the future.".into());
        }
        let mut random = [0u8; 16];
        use std::io::Read;
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let id = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let db = self.database(true)?.unwrap();
        db.transaction(|db|{db.execute("INSERT OR REPLACE INTO reminders (id,scope,pane_id,title,due,context) VALUES (?,?,?,?,?,?)",&[json!(id),ctx["scope"].clone(),json!(pane),json!(title),json!(due),json!(ctx.to_string())])?;db.execute("DELETE FROM reminders WHERE status='fired' AND due < ?",&[json!(now-7.*86400.)])?;Ok(())})?;
        Ok(id)
    }
    pub fn unread(&self, scope: &str) -> Result<HashSet<String>> {
        let Some(db) = self.database(false)? else {
            return Ok(HashSet::new());
        };
        if db
            .query(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='unread_reminders'",
                &[],
            )?
            .is_empty()
        {
            return Ok(HashSet::new());
        }
        Ok(db
            .query(
                "SELECT pane_id FROM unread_reminders WHERE scope=?",
                &[json!(scope)],
            )?
            .iter()
            .map(|v| string(v, "pane_id").into())
            .collect())
    }
    pub fn acknowledge(&self, scope: &str, pane: &str) -> Result<bool> {
        let Some(db) = self.database(false)? else {
            return Ok(false);
        };
        if db
            .query(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='unread_reminders'",
                &[],
            )?
            .is_empty()
        {
            return Ok(false);
        }
        Ok(db.execute(
            "DELETE FROM unread_reminders WHERE scope=? AND pane_id=?",
            &[json!(scope), json!(pane)],
        )? > 0)
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        if let Some(db) = self.database(false)? {
            db.execute("DELETE FROM reminders WHERE id=?", &[json!(id)])?;
        }
        Ok(())
    }
}
fn contextual(command: &mut Command, ctx: &Value) {
    for (name, key) in [
        ("HERDR_SOCKET_PATH", "socket"),
        ("HERDR_CONFIG_PATH", "config"),
    ] {
        let value = string(ctx, key);
        if value.is_empty() {
            command.env_remove(name);
        } else {
            command.env(name, value);
        }
    }
}
pub fn sync_sidebar(store: &Store, ctx: &Value, pane: &str) -> Result<()> {
    private_dir(&store.root)?;
    let _lock = lock(&store.root.join("sidebar.lock"), false)?;
    let unread = store.unread(string(ctx, "scope"))?.contains(pane);
    let mut cmd = Command::new(string(ctx, "herdr"));
    cmd.args(["pane", "report-metadata", pane, "--source", "agent-picker"]);
    if unread {
        cmd.args(["--token", "picker_alarm_unread=●"]);
    } else {
        cmd.args(["--clear-token", "picker_alarm_unread"]);
    }
    contextual(&mut cmd, ctx);
    command_output(&mut cmd, Duration::from_secs(5))?;
    Ok(())
}
fn notification_commands(reminder: &Value, ctx: &Value) -> Vec<Vec<String>> {
    let body = format!("Time to revisit: {}", string(reminder, "title"));
    let mut commands = vec![vec![
        string(ctx, "herdr").into(),
        "notification".into(),
        "show".into(),
        "Agent reminder".into(),
        "--body".into(),
        body.clone(),
        "--sound".into(),
        "done".into(),
    ]];
    if cfg!(target_os = "macos") {
        commands.push(vec!["/usr/bin/osascript".into(),"-e".into(),"on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run".into(),"Herdr Agent Picker".into(),body]);
    } else if let Some(path) = which("notify-send") {
        commands.push(vec![
            path.to_string_lossy().into_owned(),
            "Herdr Agent Picker".into(),
            body,
        ]);
    }
    commands
}
pub fn notify(reminder: &Value) -> Result<()> {
    let ctx: Value = serde_json::from_str(string(reminder, "context"))?;
    let commands = notification_commands(reminder, &ctx);
    let mut delivered = false;
    for args in commands {
        let mut cmd = Command::new(&args[0]);
        cmd.args(&args[1..]);
        contextual(&mut cmd, &ctx);
        if command_output(&mut cmd, Duration::from_secs(5)).is_ok() {
            delivered = true;
        }
    }
    if delivered {
        Ok(())
    } else {
        Err("Could not deliver reminder notification".into())
    }
}
pub fn process_due(store: &Store, now: f64, deliver: impl Fn(&Value) -> Result<()>) -> Result<()> {
    for r in store.pending(None)? {
        if string(&r, "status") != "pending"
            || r["due"]
                .as_f64()
                .unwrap_or(f64::INFINITY)
                .max(r["retry_at"].as_f64().unwrap_or(0.))
                > now
        {
            continue;
        }
        let db = store.database(true)?.unwrap();
        let claimed = db.transaction(|db| {
            let claimed = db.execute(
                "UPDATE reminders SET status='delivering' WHERE id=? AND status='pending'",
                &[r["id"].clone()],
            )? > 0;
            if claimed && r["attempts"].as_f64().unwrap_or(0.) == 0. {
                db.execute(
                    "INSERT OR IGNORE INTO unread_reminders (scope,pane_id) VALUES (?,?)",
                    &[r["scope"].clone(), r["pane_id"].clone()],
                )?;
            }
            Ok(claimed)
        })?;
        if !claimed {
            continue;
        }
        let sidebar = || -> Result<()> {
            sync_sidebar(
                store,
                &serde_json::from_str(string(&r, "context"))?,
                string(&r, "pane_id"),
            )
        };
        if let Err(e) = sidebar() {
            eprintln!("Alarm sidebar: {e}");
        }
        match deliver(&r) {
            Ok(()) => {
                db.execute(
                    "UPDATE reminders SET status='fired', error=NULL WHERE id=?",
                    &[r["id"].clone()],
                )?;
            }
            Err(e) => {
                db.execute("UPDATE reminders SET status='pending', retry_at=?, attempts=attempts+1, error=? WHERE id=?",&[json!(now+60.),json!(e.to_string()),r["id"].clone()])?;
            }
        }
    }
    Ok(())
}
pub fn ensure_worker(store: &Store) -> Result<()> {
    if store.pending(None)?.is_empty() {
        return Ok(());
    }
    let Some(probe) = lock(&store.root.join("reminders-worker.lock"), true)? else {
        return Ok(());
    };
    drop(probe);
    let logpath = store.root.join("reminders-worker.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&logpath)?;
    fs::set_permissions(logpath, fs::Permissions::from_mode(0o600))?;
    let mut cmd = Command::new(executable()?);
    cmd.arg("worker")
        .arg(&store.root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}
pub fn worker(root: &Path) -> Result<()> {
    let store = Store::new(root.into());
    let Some(worker_lock) = lock(&root.join("reminders-worker.lock"), true)? else {
        return Ok(());
    };
    if let Some(db) = store.database(false)? {
        db.execute(
            "UPDATE reminders SET status='pending' WHERE status='delivering'",
            &[],
        )?;
    }
    while !store.pending(None)?.is_empty() {
        process_due(&store, now(), notify)?;
        std::thread::sleep(Duration::from_secs(1));
    }
    drop(worker_lock);
    ensure_worker(&store)
}
const PRESETS: [&str; 8] = [
    "5 minutes",
    "10 minutes",
    "15 minutes",
    "30 minutes",
    "1 hour",
    "2 hours",
    "Tomorrow 09:00",
    "Custom time...",
];
pub fn menu(pane: &str) -> Result<()> {
    if pane.is_empty() || pane == "@section" {
        return Ok(());
    }
    let action = || -> Result<()> {
        let ctx = context();
        let store = Store::new(state_dir());
        let result = request(&["api", "snapshot"])?;
        let a = result["snapshot"]["agents"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|a| string(a, "pane_id") == pane && !string(a, "agent").is_empty())
            })
            .ok_or("This agent is no longer available.")?;
        let title = label(
            ["terminal_title_stripped", "title", "name", "agent"]
                .iter()
                .map(|k| string(a, k))
                .find(|s| !s.is_empty())
                .unwrap_or(""),
        );
        let current = store
            .pending(Some(string(&ctx, "scope")))?
            .into_iter()
            .find(|r| string(r, "pane_id") == pane);
        let mut rows: Vec<_> = PRESETS
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}\t[{}] {s}", i + 1, i + 1))
            .collect();
        if current.is_some() {
            rows.push("9\t[9] Cancel current alarm".into());
        }
        let selected = Dialog::new()?.choose(
            &rows,
            &format!("{title}\nj/k ↑/↓: move  1-9 / Enter: set  Esc: back"),
            "Alarm",
        )?;
        let Some(selected) = selected else {
            return Ok(());
        };
        if selected == "9" {
            if let Some(r) = current {
                store.cancel(string(&r, "id"))?;
            }
            return Ok(());
        }
        let index = selected.parse::<usize>()?;
        let now = now();
        let due = match index {
            1..=6 => now + [300., 600., 900., 1800., 3600., 7200.][index - 1],
            7 => {
                let t = local(now);
                calendar([t.tm_year + 1900, t.tm_mon + 1, t.tm_mday + 1, 9, 0], false)?
            }
            8 => {
                let Some(due) =
                    Dialog::new()?.date_time(current.as_ref().and_then(|r| r["due"].as_f64()))?
                else {
                    return Ok(());
                };
                due
            }
            _ => return Err("Invalid preset".into()),
        };
        store.schedule(pane, &title, due, &ctx, now)?;
        ensure_worker(&store)
    };
    dialog_error(action())
}
pub fn manage() -> Result<()> {
    let action = || -> Result<()> {
        let store = Store::new(state_dir());
        let items = store.pending(None)?;
        if items.is_empty() {
            return Dialog::new()?.message("No scheduled alarms.");
        }
        let rows: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, r)| {
                format!(
                    "{}\t{} {}  {}{}",
                    string(r, "id"),
                    if i < 9 {
                        format!("[{}]", i + 1)
                    } else {
                        "   ".into()
                    },
                    date_time(r["due"].as_f64().unwrap()),
                    label(string(r, "title")),
                    if r["error"].is_null() {
                        ""
                    } else {
                        " (delivery failed; retrying)"
                    }
                )
            })
            .collect();
        if let Some(id) = Dialog::new()?.choose(
            &rows,
            "j/k ↑/↓: move  Enter: cancel  Esc: back",
            "Scheduled alarms",
        )? {
            store.cancel(&id)?;
        }
        Ok(())
    };
    dialog_error(action())
}
fn dialog_error(result: Result<()>) -> Result<()> {
    if let Err(e) = result {
        Dialog::new()?.message(&e.to_string())?;
        return Err(e);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sidebar_metadata_uses_stored_context_and_current_unread() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.0.clone());
        let fake = dir.0.join("herdr-fixture");
        let log = dir.0.join("args");
        let script = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" \"${{HERDR_SOCKET_PATH-unset}}\" \"${{HERDR_CONFIG_PATH-unset}}\" > {}\n", quote(log.to_str().unwrap()));
        fs::write(&fake, script).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        let ctx = json!({"herdr":fake.to_str().unwrap(), "scope":"test", "socket":"/fixture.sock", "config":""});
        let db = store.database(true).unwrap().unwrap();
        db.execute(
            "INSERT INTO unread_reminders (scope,pane_id) VALUES (?,?)",
            &[json!("test"), json!("p")],
        )
        .unwrap();
        sync_sidebar(&store, &ctx, "p").unwrap();
        assert_eq!(
            fs::read_to_string(&log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "pane",
                "report-metadata",
                "p",
                "--source",
                "agent-picker",
                "--token",
                "picker_alarm_unread=●",
                "/fixture.sock",
                "unset"
            ]
        );
        store.acknowledge("test", "p").unwrap();
        sync_sidebar(&store, &ctx, "p").unwrap();
        assert!(fs::read_to_string(&log)
            .unwrap()
            .contains("--clear-token\npicker_alarm_unread\n"));
    }
    #[test]
    fn notification_arguments_are_literal_and_context_isolated() {
        let title = "Quotes \" and $(literal); 'test'";
        let ctx = json!({"herdr":"/fixture/herdr", "socket":"/fixture/session", "config":""});
        let commands = notification_commands(&json!({"title":title}), &ctx);
        assert_eq!(
            commands[0],
            [
                "/fixture/herdr",
                "notification",
                "show",
                "Agent reminder",
                "--body",
                &format!("Time to revisit: {title}"),
                "--sound",
                "done"
            ]
        );
        if cfg!(target_os = "macos") {
            assert_eq!(commands.len(), 2);
            assert_eq!(
                commands[1].last().unwrap(),
                &format!("Time to revisit: {title}")
            );
            assert!(!commands[1][2].contains(title));
        }
        let mut cmd = Command::new("/fixture/herdr");
        contextual(&mut cmd, &ctx);
        let values: std::collections::BTreeMap<_, _> = cmd.get_envs().collect();
        assert_eq!(
            values[std::ffi::OsStr::new("HERDR_SOCKET_PATH")],
            Some(std::ffi::OsStr::new("/fixture/session"))
        );
        assert_eq!(values[std::ffi::OsStr::new("HERDR_CONFIG_PATH")], None);
    }
    #[test]
    fn durable_replacement_scope_retry_and_ack() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.0.clone());
        assert!(store.pending(None).unwrap().is_empty());
        assert!(!dir.0.join("reminders.sqlite3").exists());
        let ctx = json!({"scope":"a","socket":"","config":"","herdr":"/nonexistent-fixture"});
        let first = store.schedule("p", "first", 1100., &ctx, 1000.).unwrap();
        let id = store
            .schedule("p", "Quotes \" and $(literal)", 1200., &ctx, 1000.)
            .unwrap();
        store.cancel(&first).unwrap();
        assert_eq!(store.pending(None).unwrap().len(), 1);
        let other = store
            .schedule("p", "other", 1300., &json!({"scope":"b"}), 1000.)
            .unwrap();
        assert_eq!(store.pending(Some("a")).unwrap().len(), 1);
        store.cancel(&other).unwrap();
        process_due(&store, 1199., |_| panic!("early")).unwrap();
        process_due(&store, 1200., |_| Err("failure".into())).unwrap();
        assert!(store.unread("a").unwrap().contains("p"));
        assert!(!store.acknowledge("b", "p").unwrap());
        assert!(store.acknowledge("a", "p").unwrap());
        let r = &store.pending(None).unwrap()[0];
        assert_eq!(r["retry_at"], 1260.);
        assert_eq!(r["attempts"], 1.);
        process_due(&store, 1259., |_| panic!("early retry")).unwrap();
        process_due(&store, 1260., |r| {
            assert_eq!(r["id"], id);
            Ok(())
        })
        .unwrap();
        assert!(store.pending(None).unwrap().is_empty());
        assert!(store.unread("a").unwrap().is_empty());
        process_due(&store, 2000., |_| panic!("duplicate")).unwrap();
        assert_eq!(
            fs::metadata(dir.0.join("reminders.sqlite3"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
