//! The words on the approval dock: a question for the title and what exactly will run.
//!
//! Pure functions of the request, so they are unit-tested without a window.

use std::path::Path;

use newjen_core::PermissionRequest;
use newjen_core::transcript::{display_name, summarize};
use serde_json::Value;

use crate::format::{cap, fit_rows};

/// How many lines of detail the dock shows at most before it cuts the rest (for tools other
/// than Bash, whose arguments are one line of JSON or a file path).
const DETAIL_LINES: usize = 8;
/// A character budget on top of the line cap, for one very long line.
const DETAIL_CHARS: usize = 800;

/// The space the dock has for its detail: columns of monospace text and wrapped rows. The chat
/// view works it out from the window size (see `ChatView::dock_room`), so a long command is
/// cut to what fits and the dock's buttons always stay on screen.
#[derive(Clone, Copy, Debug)]
pub struct Room {
    pub cols: usize,
    pub rows: usize,
}

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
/// comment), the file for edits, otherwise the arguments as compact JSON. Cut to `room`, so a
/// huge command cannot push the buttons off screen. A long command keeps its first and last
/// lines with the middle cut, so the end of the command (often the part that actually runs
/// things) is always visible before you approve it.
pub fn detail(request: &PermissionRequest, cwd: Option<&Path>, room: Room) -> String {
    if request.tool_name == "Bash"
        && let Some(command) = request.input.get("command").and_then(Value::as_str)
    {
        // The purpose gets one row, unless the room is so small the command needs it.
        let purpose = purpose(request).filter(|_| room.rows >= 3);
        let command_rows = room.rows - usize::from(purpose.is_some());
        let mut text = fit_rows(&format!("$ {command}"), room.cols, command_rows);
        if let Some(purpose) = purpose {
            text.push('\n');
            text.push_str(&fit_rows(&format!("# {purpose}"), room.cols, 1));
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
    fit_rows(
        &cap(&text, DETAIL_LINES, DETAIL_CHARS),
        room.cols,
        room.rows,
    )
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
    use crate::format::text_rows;
    use serde_json::json;

    /// A big dock: wide enough that no line wraps, 8 rows.
    const ROOMY: Room = Room {
        cols: 1_000,
        rows: 8,
    };

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
        assert_eq!(detail(&ask, None, ROOMY), "$ ls -la\n# List");

        let mut described = request("Bash", json!({"command": "touch a.txt"}));
        described.description = Some("Create an empty file".to_string());
        assert_eq!(
            detail(&described, None, ROOMY),
            "$ touch a.txt\n# Create an empty file"
        );
    }

    #[test]
    fn edits_name_the_file_relative_to_the_project() {
        let cwd = Path::new("/work/app");
        let ask = request("Edit", json!({"file_path": "/work/app/src/main.rs"}));
        assert_eq!(title(&ask, Some(cwd)), "Allow Claude to edit src/main.rs?");
        assert_eq!(detail(&ask, Some(cwd), ROOMY), "src/main.rs");
    }

    #[test]
    fn other_tools_show_compact_json() {
        let ask = request("mcp__linear__search", json!({"label": "billing"}));
        assert_eq!(title(&ask, None), "Allow Claude to use linear · search?");
        assert_eq!(detail(&ask, None, ROOMY), r#"{"label":"billing"}"#);

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
            detail(&ask, None, ROOMY),
            "$ echo 1\necho 2\necho 3\n… 13 more lines …\necho 17\necho 18\necho 19\necho 20"
        );
    }

    #[test]
    fn detail_fits_the_room_at_the_smallest_window() {
        // A long heredoc with long lines, as at 640x480: about 75 columns and 6 rows.
        let command = (1..=40)
            .map(|n| format!("echo line {n} {}", "word ".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n")
            + "\nrm -rf build";
        let mut ask = request("Bash", json!({ "command": command }));
        ask.description = Some("Rebuild everything from scratch, slowly".to_string());
        let room = Room { cols: 75, rows: 6 };
        let shown = detail(&ask, None, room);
        assert!(text_rows(&shown, room.cols) <= room.rows, "{shown}");
        assert!(shown.starts_with("$ echo line 1 "), "{shown}");
        // The line that does the damage and the purpose stay visible.
        assert!(shown.contains("\nrm -rf build\n# Rebuild"), "{shown}");
    }
}
