use crate::{dialog::Dialog, util::*};
use serde_json::{json, Value};
use std::path::Path;

// Deliberately exclude mutable labels, status, revision and list position.
#[derive(Debug, PartialEq)]
struct Target {
    pane: String,
    kind: String,
    terminal: String,
    session: Value,
    name: String,
    foreground: Value,
}
impl Target {
    fn read(pane: &str) -> Result<Self> {
        let agent = request(&["agent", "get", pane])?["agent"].clone();
        let processes = request(&["pane", "process-info", "--pane", pane])?;
        Self::from_values(pane, &agent, &processes["process_info"])
    }

    fn from_values(pane: &str, agent: &Value, processes: &Value) -> Result<Self> {
        let kind = string(agent, "agent");
        let terminal = string(agent, "terminal_id");
        let group = processes["foreground_process_group_id"].as_u64();
        let shell = processes["shell_pid"].as_u64();
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
                "Agent is gone or its foreground identity cannot be verified. Nothing sent.".into(),
            );
        }
        let foreground =
            foreground.ok_or("Cannot verify agent foreground process. Nothing sent.")?;
        let mut identity = vec![];
        let mut recognized = false;
        for process in foreground {
            let pid = process["pid"]
                .as_u64()
                .filter(|pid| *pid > 0)
                .ok_or("Missing foreground PID. Nothing sent.")?;
            if Some(pid) == shell {
                return Err("Shell is in the foreground. Nothing sent.".into());
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
            return Err("Agent is not verifiably in the foreground. Nothing sent.".into());
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

    fn recipient(&self) -> String {
        let name = if self.name.is_empty() {
            self.kind.clone()
        } else {
            format!("{} ({})", self.name, self.kind)
        };
        format!(
            "{} / pid {} / {}",
            self.pane, self.foreground["group"], name
        )
    }

    fn send(&self, text: &str) -> Result<()> {
        if *self != Self::read(&self.pane)? {
            return Err("Agent exited or was replaced. Nothing sent.".into());
        }
        // One argv-based, agent-validated ordered submission. Never fall back to
        // pane input or retry: an error/timeout can mean partial delivery.
        request(&["agent", "prompt", &self.pane, "--", text]).map_err(
            |e| -> Box<dyn std::error::Error> {
                format!("Delivery uncertain; not retried: {e}").into()
            },
        )?;
        Ok(())
    }
}

pub fn open(pane: &str) -> Result<()> {
    if pane.is_empty() || pane == "@section" {
        return Ok(());
    }
    let mut dialog = Dialog::new()?;
    let mut action = || -> Result<()> {
        let target = Target::read(pane)?;
        if let Some(text) = dialog.compose(&target.recipient())? {
            target.send(&text)?;
        }
        Ok(())
    };
    if let Err(e) = action() {
        dialog.composer_error(&e.to_string())?;
    }
    Ok(())
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
