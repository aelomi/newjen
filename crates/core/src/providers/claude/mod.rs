mod approvals;
pub(crate) mod controller;
pub mod events;
pub mod presentation;

use serde_json::Value;
use std::path::Path;

/// A friendlier tool name: MCP tools arrive as `mcp__<server>__<tool>` and become
/// `<server> · <tool>`. Other names are returned unchanged.
pub fn display_name(tool_name: &str) -> String {
    if let Some(rest) = tool_name.strip_prefix("mcp__")
        && let Some((server, tool)) = rest.split_once("__")
    {
        return format!("{server} · {tool}");
    }
    tool_name.to_string()
}

/// A one-line summary of a tool call for its card, e.g. the Bash command or the file it edits.
/// File paths inside `cwd` are shown relative to it.
pub fn summarize(tool_name: &str, input: &Value, cwd: Option<&Path>) -> String {
    let field = |name: &str| input.get(name).and_then(Value::as_str);
    let summary = match tool_name {
        "Bash" => field("command").map(|command| command.lines().next().unwrap_or("").to_string()),
        "Read" | "Edit" | "Write" | "MultiEdit" => {
            field("file_path").map(|path| relative(path, cwd))
        }
        "NotebookEdit" => field("notebook_path")
            .or_else(|| field("file_path"))
            .map(|path| relative(path, cwd)),
        "Glob" | "Grep" => field("pattern").map(str::to_string),
        "WebFetch" => field("url").map(str::to_string),
        "WebSearch" => field("query").map(str::to_string),
        "Task" | "Agent" => field("description").map(str::to_string),
        _ => None,
    };
    summary
        .or_else(|| first_string_field(input))
        .unwrap_or_default()
}

/// The first string value in a JSON object, as a last-resort summary.
fn first_string_field(input: &Value) -> Option<String> {
    let object = input.as_object()?;
    object
        .values()
        .find_map(Value::as_str)
        .map(|text| text.lines().next().unwrap_or("").to_string())
}

/// `path` relative to `cwd` when it is inside it; otherwise unchanged.
fn relative(path: &str, cwd: Option<&Path>) -> String {
    let Some(cwd) = cwd else {
        return path.to_string();
    };
    match Path::new(path).strip_prefix(cwd) {
        Ok(inside) if !inside.as_os_str().is_empty() => inside.display().to_string(),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn display_name_shortens_mcp_tools() {
        assert_eq!(
            display_name("mcp__linear__list_issues"),
            "linear · list_issues"
        );
        assert_eq!(display_name("Bash"), "Bash");
        assert_eq!(display_name("mcp__broken"), "mcp__broken");
    }

    #[test]
    fn summarize_common_tools() {
        let cwd = Path::new("/tmp/project");
        assert_eq!(
            summarize("Bash", &json!({"command": "ls -la\necho hi"}), None),
            "ls -la"
        );
        assert_eq!(
            summarize(
                "Edit",
                &json!({"file_path": "/tmp/project/src/main.rs"}),
                Some(cwd)
            ),
            "src/main.rs"
        );
        assert_eq!(
            summarize("Read", &json!({"file_path": "/etc/hosts"}), Some(cwd)),
            "/etc/hosts"
        );
        assert_eq!(
            summarize("Grep", &json!({"pattern": "fn main"}), None),
            "fn main"
        );
        assert_eq!(
            summarize("WebFetch", &json!({"url": "https://example.com"}), None),
            "https://example.com"
        );
        assert_eq!(
            summarize("WebSearch", &json!({"query": "rust"}), None),
            "rust"
        );
        assert_eq!(
            summarize("Task", &json!({"description": "Explore"}), None),
            "Explore"
        );
        assert_eq!(
            summarize("SomethingElse", &json!({"n": 1, "a": "x"}), None),
            "x"
        );
        assert_eq!(summarize("SomethingElse", &json!({}), None), "");
    }
}
