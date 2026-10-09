//! Function tools the assistant may call. Results append to JSON files in the
//! app settings directory.
use anyhow::{Context, Result, bail};
use openai_realtime::{ToolSpec, function_tool};
use pebble_core::config::save_json;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const ADD_TODO: &str = "add_todo";
pub const ADD_MEMO: &str = "add_memo";

pub fn specs() -> Vec<ToolSpec> {
    vec![
        function_tool(
            ADD_TODO,
            "ユーザーのTODOリストに項目を追加する",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "やることの短い題名"},
                    "due": {"type": "string", "description": "期限があれば ISO 8601 の日付"}
                },
                "required": ["title"],
                "additionalProperties": false
            }),
        ),
        function_tool(
            ADD_MEMO,
            "ユーザーが覚えておきたい内容をメモとして保存する",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "メモ本文"}
                },
                "required": ["text"],
                "additionalProperties": false
            }),
        ),
    ]
}

pub struct Tools {
    directory: PathBuf,
}
impl Tools {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    /// Run a tool and return the JSON string handed back to the model.
    pub fn execute(&self, name: &str, arguments: &str) -> Result<String> {
        let arguments: Value =
            serde_json::from_str(arguments).context("Tool arguments are not JSON")?;
        let (file, entry) = match name {
            ADD_TODO => {
                let title = arguments["title"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .context("add_todo needs a title")?;
                let mut entry = json!({"title": title.trim(), "done": false});
                if let Some(due) = arguments["due"].as_str() {
                    entry["due"] = json!(due);
                }
                ("todos.json", entry)
            }
            ADD_MEMO => {
                let text = arguments["text"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .context("add_memo needs text")?;
                ("memos.json", json!({"text": text.trim()}))
            }
            other => bail!("Unknown tool {other}"),
        };
        let id = append(&self.directory.join(file), entry)?;
        Ok(json!({"ok": true, "id": id}).to_string())
    }
}
fn append(path: &Path, mut entry: Value) -> Result<u64> {
    let mut items: Vec<Value> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Corrupt tool store")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e.into()),
    };
    let id = items
        .iter()
        .filter_map(|item| item["id"].as_u64())
        .max()
        .unwrap_or(0)
        + 1;
    entry["id"] = json!(id);
    entry["created_at"] = json!(chrono::Local::now().to_rfc3339());
    items.push(entry);
    save_json(path, &Value::Array(items))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todos_and_memos_append_with_increasing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools::new(dir.path().into());
        let first = tools
            .execute(ADD_TODO, r#"{"title":"牛乳を買う","due":"2026-10-10"}"#)
            .unwrap();
        let second = tools.execute(ADD_TODO, r#"{"title":"掃除"}"#).unwrap();
        let first: Value = serde_json::from_str(&first).unwrap();
        let second: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(
            (first["ok"].as_bool(), first["id"].as_u64()),
            (Some(true), Some(1))
        );
        assert_eq!(
            (second["ok"].as_bool(), second["id"].as_u64()),
            (Some(true), Some(2))
        );
        let todos: Vec<Value> =
            serde_json::from_slice(&std::fs::read(dir.path().join("todos.json")).unwrap()).unwrap();
        assert_eq!(todos[0]["due"], "2026-10-10");
        assert_eq!(todos[1]["title"], "掃除");
        assert!(todos[1].get("due").is_none());
        tools
            .execute(ADD_MEMO, r#"{"text":" 会議は火曜 "}"#)
            .unwrap();
        let memos: Vec<Value> =
            serde_json::from_slice(&std::fs::read(dir.path().join("memos.json")).unwrap()).unwrap();
        assert_eq!(memos[0]["text"], "会議は火曜");
        assert_eq!(memos[0]["id"], 1);
    }

    #[test]
    fn invalid_calls_are_errors_and_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools::new(dir.path().into());
        assert!(tools.execute(ADD_TODO, r#"{"title":"  "}"#).is_err());
        assert!(tools.execute("open_door", "{}").is_err());
        assert!(tools.execute(ADD_MEMO, "not json").is_err());
        assert!(!dir.path().join("todos.json").exists());
    }

    #[test]
    fn specs_declare_both_functions() {
        let names: Vec<_> = specs().into_iter().map(|t| t.name).collect();
        assert_eq!(names, [ADD_TODO, ADD_MEMO]);
    }
}
