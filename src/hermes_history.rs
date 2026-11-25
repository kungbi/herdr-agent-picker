use crate::{sqlite::Database, util::Result};
use serde_json::{json, Value};
use std::path::Path;

// A binding is carried with every cached semantic frame and checked again at paint time.
pub fn binding(pane: &str) -> Option<Value> {
    let (home, session) = crate::hermes_binding::resolve(pane)?;
    Some(json!([home.to_str()?, session]))
}

pub fn valid(pane: &str, expected: &Value) -> bool {
    !expected.is_null() && binding(pane).as_ref() == Some(expected)
}

pub fn read(pane: &str, width: usize) -> Option<(Value, Vec<String>)> {
    let owner = binding(pane)?;
    let home = Path::new(owner[0].as_str()?);
    let session = owner[1].as_str()?;
    let text = read_session(home, session).ok()?;
    if !valid(pane, &owner) {
        return None;
    }
    Some((owner, crate::preview::visual_rows(&text, width.max(1))))
}

fn read_session(home: &Path, session: &str) -> Result<String> {
    let db = Database::open_readonly(&home.join("state.db"))?;
    // shortcut: only the latest 64 messages and 16 KiB per message are available; paginate by ID for older history.
    let mut rows = db.query_bounded(
        "SELECT role, CAST(substr(CAST(content AS BLOB), 1, 16384) AS TEXT) AS content \
         FROM messages WHERE session_id = ? AND role IN ('user', 'assistant') \
         AND active = 1 AND _compressed_summary = 0 AND COALESCE(display_kind, '') = '' \
         AND tool_calls IS NULL AND content IS NOT NULL ORDER BY id DESC LIMIT 64",
        &[json!(session)],
    )?;
    rows.reverse();
    let mut output = String::new();
    for row in rows {
        let Some(content) = row["content"].as_str() else {
            continue;
        };
        let text = if let Some(encoded) = content.strip_prefix("\0json:") {
            match serde_json::from_str::<Value>(encoded) {
                Ok(Value::String(text)) => text,
                Ok(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| {
                        (part["type"].as_str() == Some("text"))
                            .then(|| part["text"].as_str())
                            .flatten()
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => continue,
            }
        } else if !content.contains('\0') {
            content.to_owned()
        } else {
            continue;
        };
        if text.contains('\0')
            || text.contains("[END OF PRIOR CONTEXT — COMPACTION SUMMARY BELOW]")
            || text.trim_start().starts_with("[CONTEXT SUMMARY]")
            || text.trim_start().starts_with("[CONTEXT COMPACTION")
            || text
                .trim_start()
                .starts_with("[IMPORTANT: The user has invoked the ")
            || text.trim_start().starts_with("[IMPORTANT: The \"")
            || text.trim_start().starts_with("[The following is a summary")
        {
            continue;
        }
        let text = crate::preview::public_text(&text);
        if !text.trim().is_empty() {
            output.push_str(&text);
            output.push_str("\n\n");
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_session_and_public_rows_only() {
        let dir = std::env::temp_dir().join(format!("picker-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("state.db")).unwrap();
        db.execute("CREATE TABLE messages(id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, active INTEGER, _compressed_summary INTEGER, display_kind TEXT, tool_calls TEXT)", &[]).unwrap();
        for (session, role, content, active, summary, kind, calls) in [
            ("a", "user", "question", 1, 0, "", ""),
            ("a", "assistant", "answer", 1, 0, "", ""),
            ("b", "user", "other session", 1, 0, "", ""),
            ("a", "assistant", "hidden", 1, 0, "hidden", ""),
            ("a", "assistant", "inactive", 0, 0, "", ""),
            ("a", "user", "summary", 1, 1, "", ""),
            ("a", "tool", "tool result", 1, 0, "", ""),
            ("a", "assistant", "tool call", 1, 0, "", "calls"),
            (
                "a",
                "user",
                "[CONTEXT COMPACTION — REFERENCE ONLY] secret",
                1,
                0,
                "",
                "",
            ),
            (
                "a",
                "user",
                "[IMPORTANT: The user has invoked the skill] secret",
                1,
                0,
                "",
                "",
            ),
        ] {
            db.execute("INSERT INTO messages(session_id,role,content,active,_compressed_summary,display_kind,tool_calls) VALUES(?,?,?,?,?,?,NULLIF(?,''))", &[json!(session),json!(role),json!(content),json!(active),json!(summary),json!(kind),json!(calls)]).unwrap();
        }
        db.execute("INSERT INTO messages(session_id,role,content,active,_compressed_summary) VALUES ('a','user',char(0)||'json:'||?,1,0)", &[json!("[{\"type\":\"text\",\"text\":\"structured question\"},{\"type\":\"reasoning\",\"text\":\"private thought\"}]")]).unwrap();
        let result = read_session(&dir, "a").unwrap();
        assert!(result.contains("question") && result.contains("answer"));
        assert!(result.contains("structured question"));
        for forbidden in [
            "other session",
            "hidden",
            "inactive",
            "summary",
            "tool result",
            "tool call",
            "private thought",
            "secret",
        ] {
            assert!(!result.contains(forbidden));
        }
        assert!(!read_session(&dir, "b").unwrap().contains("question"));
        assert!(Database::open_readonly(&dir.join("missing.db")).is_err());
        assert!(!dir.join("missing.db").exists());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
