pub mod events;

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
