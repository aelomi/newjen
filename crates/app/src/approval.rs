//! The words on the approval dock: a question for the title and what exactly will run.
//!
//! Pure functions of the request, so they are unit-tested without a window.

use std::path::Path;

use newjen_core::PermissionRequest;
use newjen_core::transcript::{display_name, summarize};
use serde_json::Value;

use crate::format::{cap, cap_ends};

/// How many lines of detail the dock shows before it cuts the rest.
const DETAIL_LINES: usize = 8;
/// A character budget on top of the line cap, for one very long line.
const DETAIL_CHARS: usize = 800;
/// A long Bash command shows its first and last lines with the middle cut, so the end of the
/// command (often the part that actually runs things) is always visible before you approve it.
const COMMAND_HEAD: usize = 3;
const COMMAND_TAIL: usize = 4;
/// Each command line is cut in its middle past this many characters (about two rows).
const COMMAND_LINE_CHARS: usize = 160;
/// Claude's one-line reason for the command, cut past this many characters.
const PURPOSE_CHARS: usize = 160;

/// Tools that change a file; their dock names the file.
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The dock's question, e.g. "Allow Bash to run this command?".
pub fn title(request: &PermissionRequest, cwd: Option<&Path>) -> String {
    if request.tool_name == "Bash" {
        return "Allow Bash to run this command?".to_string();
    }
    if EDIT_TOOLS.contains(&request.tool_name.as_str()) {
        let file = summarize(&request.tool_name, &request.input, cwd);
        if !file.is_empty() {
            return format!("Allow Claude to edit {file}?");
        }
    }
    format!("Allow Claude to use {}?", tool_label(request))
}

/// What will run: `$ <command>` for Bash (with Claude's stated purpose under it as a `#`
/// comment), the file for edits, otherwise the arguments as compact JSON. Capped so a huge
/// command cannot push the buttons off screen.
pub fn detail(request: &PermissionRequest, cwd: Option<&Path>) -> String {
    if request.tool_name == "Bash"
        && let Some(command) = request.input.get("command").and_then(Value::as_str)
    {
        let mut text = cap_ends(
            &format!("$ {command}"),
            COMMAND_HEAD,
            COMMAND_TAIL,
            COMMAND_LINE_CHARS,
        );
        if let Some(purpose) = purpose(request) {
            text.push_str("\n# ");
            text.push_str(&cap(purpose, 1, PURPOSE_CHARS));
        }
        return text;
    }
    let specific = if EDIT_TOOLS.contains(&request.tool_name.as_str()) {
        Some(summarize(&request.tool_name, &request.input, cwd)).filter(|file| !file.is_empty())
    } else {
        None
    };
    // Anything else (or a request missing the expected field): the raw arguments.
    let text =
        specific.unwrap_or_else(|| serde_json::to_string(&request.input).unwrap_or_default());
    cap(&text, DETAIL_LINES, DETAIL_CHARS)
}

/// Claude's one-line reason for a call, e.g. "List files in the current directory": the CLI's
/// description, else the tool input's own `description` field.
fn purpose(request: &PermissionRequest) -> Option<&str> {
    request
        .description
        .as_deref()
        .or_else(|| request.input.get("description").and_then(Value::as_str))
        .and_then(|text| text.lines().next())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// The tool's name as its card in the conversation shows it (`mcp__server__tool` →
/// `server · tool`), so the dock and the card always agree.
fn tool_label(request: &PermissionRequest) -> String {
    display_name(&request.tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(tool_name: &str, input: Value) -> PermissionRequest {
        PermissionRequest {
            request_id: "r1".to_string(),
            tool_use_id: Some("t1".to_string()),
            tool_name: tool_name.to_string(),
            display_name: tool_name.to_string(),
            input,
            description: None,
            suggestions: Vec::new(),
        }
    }

    #[test]
    fn bash_shows_the_command() {
        let ask = request("Bash", json!({"command": "ls -la", "description": "List"}));
        assert_eq!(title(&ask, None), "Allow Bash to run this command?");
        assert_eq!(detail(&ask, None), "$ ls -la\n# List");

        let mut described = request("Bash", json!({"command": "touch a.txt"}));
        described.description = Some("Create an empty file".to_string());
        assert_eq!(
            detail(&described, None),
            "$ touch a.txt\n# Create an empty file"
        );
    }

    #[test]
    fn edits_name_the_file_relative_to_the_project() {
        let cwd = Path::new("/work/app");
        let ask = request("Edit", json!({"file_path": "/work/app/src/main.rs"}));
        assert_eq!(title(&ask, Some(cwd)), "Allow Claude to edit src/main.rs?");
        assert_eq!(detail(&ask, Some(cwd)), "src/main.rs");
    }

    #[test]
    fn other_tools_show_compact_json() {
        let ask = request("mcp__linear__search", json!({"label": "billing"}));
        assert_eq!(title(&ask, None), "Allow Claude to use linear · search?");
        assert_eq!(detail(&ask, None), r#"{"label":"billing"}"#);

        // The same name as the tool's card, even when the CLI calls it something else.
        let mut named = request("WebFetch", json!({"url": "https://example.com"}));
        named.display_name = "Fetch".to_string();
        assert_eq!(title(&named, None), "Allow Claude to use WebFetch?");
    }

    #[test]
    fn long_commands_keep_their_first_and_last_lines() {
        let command = (1..=20)
            .map(|n| format!("echo {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let ask = request("Bash", json!({ "command": command }));
        assert_eq!(
            detail(&ask, None),
            "$ echo 1\necho 2\necho 3\n… 13 more lines …\necho 17\necho 18\necho 19\necho 20"
        );
    }
}
