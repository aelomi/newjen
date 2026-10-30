//! The words on the approval dock: a question for the title and what exactly will run.
//!
//! Pure functions of the request, so they are unit-tested without a window.

use std::path::{Path, PathBuf};

use newjen_core::transcript::{display_name, summarize};
use newjen_core::{OnceReason, PermissionRequest, SessionScope};
use serde_json::Value;

use crate::format::{cap, fit_rows, middle, short_path};

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

/// The dock's detail text, and whether the Bash command had to be cut to fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub text: String,
    /// The Bash command was shortened. Only matters when "Always allow" remembers that exact
    /// command (see [`scope_row`]); other tools' arguments are cut freely, because their rule
    /// does not depend on them.
    pub shortened: bool,
}

/// What will run: `$ <command>` for Bash (with Claude's stated purpose under it as a `#`
/// comment), the file for edits, otherwise the arguments as compact JSON. Cut to `room`, so a
/// huge command cannot push the buttons off screen. A long command keeps its first and last
/// lines with the middle cut, so the end of the command (often the part that actually runs
/// things) is always visible before you approve it. A last row warns about `outside`, the path
/// from [`outside_path`] (passed in, so drawing a frame never touches the disk).
pub fn detail(
    request: &PermissionRequest,
    cwd: Option<&Path>,
    outside: Option<&str>,
    room: Room,
) -> Detail {
    // The warning gets its row first: it matters more than the purpose.
    let outside = outside.map(|path| outside_line(path, room.cols));
    let rows = room
        .rows
        .saturating_sub(usize::from(outside.is_some()))
        .max(1);
    let (mut text, shortened) = if request.tool_name == "Bash"
        && let Some(command) = request.input.get("command").and_then(Value::as_str)
    {
        // The purpose gets one row, unless the room is so small the command needs it.
        let purpose = purpose(request).filter(|_| rows >= 3);
        let command_rows = rows - usize::from(purpose.is_some());
        let full = format!("$ {command}");
        let mut text = fit_rows(&full, room.cols, command_rows);
        // Only the command counts as shortened: the purpose is Claude's comment, not what runs.
        let shortened = text != full;
        if let Some(purpose) = purpose {
            text.push('\n');
            text.push_str(&fit_rows(&format!("# {purpose}"), room.cols, 1));
        }
        (text, shortened)
    } else {
        let specific = if EDIT_TOOLS.contains(&request.tool_name.as_str()) {
            Some(summarize(&request.tool_name, &request.input, cwd)).filter(|file| !file.is_empty())
        } else {
            None
        };
        // Anything else (or a request missing the expected field): the raw arguments.
        let full =
            specific.unwrap_or_else(|| serde_json::to_string(&request.input).unwrap_or_default());
        // Not a command: whether this was cut never decides what "Always allow" remembers.
        let text = fit_rows(&cap(&full, DETAIL_LINES, DETAIL_CHARS), room.cols, rows);
        (text, false)
    };
    if let Some(outside) = outside {
        text.push('\n');
        text.push_str(&outside);
    }
    Detail { text, shortened }
}

/// The path the CLI says this call touches, if it lies outside `project`. This reads the disk
/// (to resolve symlinks), so the chat view calls it once per request, not on every frame.
pub fn outside_path(request: &PermissionRequest, project: &Path) -> Option<String> {
    let path = request.blocked_path.as_deref()?;
    outside_project(path, project).then(|| path.to_string())
}

/// "Outside the project: <path>", cut in its middle to one row of `cols`.
fn outside_line(path: &str, cols: usize) -> String {
    const LABEL: &str = "Outside the project: ";
    let room = cols.saturating_sub(LABEL.len()).max(12);
    format!("{LABEL}{}", short_path(path, room))
}

/// Whether `path` (absolute, or relative to the project) lies outside `project`. Symlinks are
/// resolved first: the project folder is stored resolved (`/tmp/x` is `/private/tmp/x` on
/// macOS), while the CLI reports paths as written, so a plain prefix check would call every
/// file in a `/tmp` project "outside".
fn outside_project(path: &str, project: &Path) -> bool {
    // `join` with an absolute path gives that path unchanged. Both sides are resolved, so a
    // `..` cannot make an outside path look inside.
    !resolve(&project.join(path)).starts_with(resolve(project))
}

/// `path` with symlinks (and `..`) resolved as far as it exists. The file itself often does
/// not exist yet (a `touch` asks before it creates the file), so the missing end is kept as
/// written.
fn resolve(path: &Path) -> PathBuf {
    for existing in path.ancestors() {
        if let Ok(real) = existing.canonicalize() {
            let rest = path.strip_prefix(existing).unwrap_or(Path::new(""));
            return real.join(rest);
        }
    }
    path.to_path_buf()
}

/// The quiet row under the dock that says what "Always allow" will do, before the click:
/// `before`, then `code` in the mono font (the rule that is remembered), then `after`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    pub before: String,
    pub code: Option<String>,
    pub after: String,
    /// The remembered rule could not be shown in full, so "Always allow" must be sent as
    /// allow once: nothing is remembered that the user could not read.
    pub once: bool,
}

/// The scope row for `request`'s `scope` (from `newjen_core::session_scope`).
/// `command_shortened` is [`Detail::shortened`]. The remembered rule is cut in its middle so
/// the row stays one line at about `cols` columns of the detail's text.
pub fn scope_row(
    scope: &SessionScope,
    request: &PermissionRequest,
    command_shortened: bool,
    cols: usize,
) -> ScopeRow {
    // `cut`: the answer itself must be downgraded here (core already sends allow once for the
    // `OnceOnly` cases, so they need no help).
    let once = |why: &str, cut: bool| ScopeRow {
        before: format!("Always allow works once here{why}"),
        code: None,
        after: String::new(),
        once: cut,
    };
    let rules = match scope {
        SessionScope::OnceOnly(OnceReason::Edit) => {
            return once(" — no safe rule for edits", false);
        }
        SessionScope::OnceOnly(OnceReason::NoSafeRule) => return once("", false),
        SessionScope::Remembers(rules) => rules,
    };
    // The thread keeps the rule (core saves it and gives it to every later `claude` of this
    // thread), so it lasts "for this thread", not just for this claude process.
    // In a rule, `*` matches more than this one call, so say that in words. For Bash it stands
    // for any arguments; for other tools it is a path or domain pattern.
    let after = match rules.iter().any(|rule| rule.contains('*')) {
        true if request.tool_name == "Bash" => " (any arguments) for this thread",
        true => " (a pattern) for this thread",
        false => " for this thread",
    };
    const BEFORE: &str = "Always allow remembers ";
    let room = cols.saturating_sub(BEFORE.len() + after.len()).max(12);
    // One line: a multi-line command's line breaks are shown as ↵.
    let full = rules.join(", ").replace('\n', " ↵ ");
    let code = middle(&full, room);
    // Where can the user read the whole rule? When it is exactly the Bash command, the detail
    // above shows it (this row only repeats it, cut), so what counts is whether the detail cut
    // it. Any other rule (a CLI suggestion such as `npm test *` or `domain:example.com`) is
    // shown only here, so what counts is whether this row cut it.
    let command = request.input.get("command").and_then(Value::as_str);
    let rule_is_command = request.tool_name == "Bash"
        && command.is_some_and(|command| rules.len() == 1 && rules[0] == command);
    if rule_is_command && command_shortened {
        return once(" — the command was shortened to fit", true);
    }
    if !rule_is_command && code != full {
        return once(" — the rule was shortened to fit", true);
    }
    ScopeRow {
        before: BEFORE.to_string(),
        code: Some(code),
        after: after.to_string(),
        once: false,
    }
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
            blocked_path: None,
        }
    }

    #[test]
    fn bash_shows_the_command() {
        let ask = request("Bash", json!({"command": "ls -la", "description": "List"}));
        assert_eq!(title(&ask, None), "Allow Bash to run this command?");
        assert_eq!(detail(&ask, None, None, ROOMY).text, "$ ls -la\n# List");

        let mut described = request("Bash", json!({"command": "touch a.txt"}));
        described.description = Some("Create an empty file".to_string());
        assert_eq!(
            detail(&described, None, None, ROOMY).text,
            "$ touch a.txt\n# Create an empty file"
        );
    }

    #[test]
    fn edits_name_the_file_relative_to_the_project() {
        let cwd = Path::new("/work/app");
        let ask = request("Edit", json!({"file_path": "/work/app/src/main.rs"}));
        assert_eq!(title(&ask, Some(cwd)), "Allow Claude to edit src/main.rs?");
        assert_eq!(detail(&ask, Some(cwd), None, ROOMY).text, "src/main.rs");
    }

    #[test]
    fn other_tools_show_compact_json() {
        let ask = request("mcp__linear__search", json!({"label": "billing"}));
        assert_eq!(title(&ask, None), "Allow Claude to use linear · search?");
        assert_eq!(
            detail(&ask, None, None, ROOMY).text,
            r#"{"label":"billing"}"#
        );

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
            detail(&ask, None, None, ROOMY).text,
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
        let shown = detail(&ask, None, None, room).text;
        assert!(text_rows(&shown, room.cols) <= room.rows, "{shown}");
        assert!(shown.starts_with("$ echo line 1 "), "{shown}");
        // The line that does the damage and the purpose stay visible.
        assert!(shown.contains("\nrm -rf build\n# Rebuild"), "{shown}");
    }

    #[test]
    fn a_shortened_command_is_flagged_so_always_allow_acts_once() {
        let ask = request("Bash", json!({"command": "touch notes.md"}));
        assert!(!detail(&ask, None, None, ROOMY).shortened);
        let long = (1..=40)
            .map(|n| format!("echo {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let cut = detail(
            &request("Bash", json!({ "command": long })),
            None,
            None,
            ROOMY,
        );
        assert!(cut.shortened, "{}", cut.text);
        // A long purpose is cut too, but it is only Claude's comment: not shortened.
        let mut chatty = request("Bash", json!({"command": "ls"}));
        chatty.description = Some("word ".repeat(400));
        assert!(!detail(&chatty, None, None, Room { cols: 40, rows: 3 }).shortened);

        let ask = request("Bash", json!({"command": "touch notes.md"}));
        let scope = SessionScope::Remembers(vec!["touch notes.md".to_string()]);
        let row = scope_row(&scope, &ask, true, 80);
        assert_eq!(
            row.before,
            "Always allow works once here — the command was shortened to fit"
        );
        assert!(row.once);
    }

    #[test]
    fn cut_arguments_do_not_matter_when_the_rule_does_not_use_them() {
        // An MCP call with huge arguments: the detail is cut, but the rule is just the tool.
        let ask = request("mcp__x__y", json!({"text": "word ".repeat(400)}));
        let shown = detail(&ask, None, None, Room { cols: 40, rows: 3 });
        assert!(!shown.shortened, "{}", shown.text);
        let scope = SessionScope::Remembers(vec!["x · y".to_string()]);
        let row = scope_row(&scope, &ask, shown.shortened, 80);
        assert!(!row.once);
        assert_eq!(row.code.as_deref(), Some("x · y"));

        // A CLI rule too long for its row is cut there, the only place it is shown.
        let fetch = request("WebFetch", json!({"url": "https://example.com"}));
        let long = SessionScope::Remembers(vec![format!("domain:{}.com", "a".repeat(200))]);
        let row = scope_row(&long, &fetch, false, 80);
        assert_eq!(
            row.before,
            "Always allow works once here — the rule was shortened to fit"
        );
        assert!(row.once);

        // A CLI rule for Bash that is not the command itself: also judged by this row.
        let npm = request(
            "Bash",
            json!({"command": format!("npm test {}", "x".repeat(300))}),
        );
        let short_rule = SessionScope::Remembers(vec!["npm test *".to_string()]);
        assert!(!scope_row(&short_rule, &npm, true, 80).once);
    }

    #[test]
    fn the_scope_row_says_what_always_allow_remembers() {
        let text = |row: ScopeRow| {
            let code = row.code.map(|code| format!("`{code}`")).unwrap_or_default();
            format!("{}{code}{}", row.before, row.after)
        };
        let bash = |command: &str| request("Bash", json!({ "command": command }));
        let touch = bash("touch notes.md");
        let exact = SessionScope::Remembers(vec!["touch notes.md".to_string()]);
        assert_eq!(
            text(scope_row(&exact, &touch, false, 80)),
            "Always allow remembers `touch notes.md` for this thread"
        );
        let wildcard = SessionScope::Remembers(vec!["npm test *".to_string()]);
        assert_eq!(
            text(scope_row(&wildcard, &bash("npm test --watch"), false, 80)),
            "Always allow remembers `npm test *` (any arguments) for this thread"
        );
        // Outside Bash, `*` is a path or domain pattern, not "any arguments".
        let domains = SessionScope::Remembers(vec!["domain:*.example.com".to_string()]);
        let fetch = request("WebFetch", json!({"url": "https://docs.example.com"}));
        assert_eq!(
            text(scope_row(&domains, &fetch, false, 80)),
            "Always allow remembers `domain:*.example.com` (a pattern) for this thread"
        );
        assert_eq!(
            text(scope_row(
                &SessionScope::OnceOnly(OnceReason::Edit),
                &touch,
                false,
                80
            )),
            "Always allow works once here — no safe rule for edits"
        );
        assert_eq!(
            text(scope_row(
                &SessionScope::OnceOnly(OnceReason::NoSafeRule),
                &touch,
                false,
                80
            )),
            "Always allow works once here"
        );
        // A long command is cut in its middle to keep the row to one line; the detail above
        // shows it in full, so "Always allow" still remembers it.
        let command = format!("echo {}done", "x".repeat(200));
        let long = SessionScope::Remembers(vec![command.clone()]);
        let row = scope_row(&long, &bash(&command), false, 80);
        assert!(!row.once);
        let code = row.code.expect("a rule");
        assert!(
            code.starts_with("echo x") && code.ends_with("done"),
            "{code}"
        );
        assert!(code.chars().count() < 80, "{code}");
    }

    #[test]
    fn a_path_outside_the_project_gets_its_own_row() {
        // A project under a symlinked folder, stored resolved as `main.rs` does it: macOS's
        // temp folder is `/var/...`, really `/private/var/...`.
        let raw = std::env::temp_dir().join(format!("newjen-outside-{}", std::process::id()));
        std::fs::create_dir_all(&raw).expect("temp project");
        let project = raw.canonicalize().expect("resolved project");
        let mut ask = request("Bash", json!({"command": "touch new.txt"}));

        // Inside, written through the symlink, and not created yet: no row.
        ask.blocked_path = Some(raw.join("new.txt").display().to_string());
        assert_eq!(outside_path(&ask, &project), None);

        ask.blocked_path = Some("/elsewhere/notes/new.txt".to_string());
        let outside = outside_path(&ask, &project);
        assert_eq!(outside.as_deref(), Some("/elsewhere/notes/new.txt"));
        // `detail` itself only formats the path it is given: no disk access while drawing.
        assert_eq!(
            detail(&ask, Some(&project), outside.as_deref(), ROOMY).text,
            "$ touch new.txt\nOutside the project: /elsewhere/notes/new.txt"
        );
        assert_eq!(
            detail(&ask, Some(&project), None, ROOMY).text,
            "$ touch new.txt"
        );
        let _ = std::fs::remove_dir_all(&raw);
    }
}
