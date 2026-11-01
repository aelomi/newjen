//! The words on the approval dock: a question for the title and what exactly will run.
//!
//! Pure functions of the request, so they are unit-tested without a window.

use std::path::{Path, PathBuf};

use newjen_core::{
    ApprovalKind, ApprovalRisk, ApprovalView, OnceReason, RememberScope, RuleMeaning,
};

use crate::format::{cap, fit_rows, middle, short_path};
use crate::ui::IconName;

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

/// The adapter supplies the question with the action's display name.
pub fn title(request: &ApprovalView) -> String {
    request.question.clone()
}

/// The dock's detail text, and whether the Bash command had to be cut to fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub text: String,
    /// The Bash command was shortened. Only matters when "Allow in this thread" remembers that exact
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
pub fn detail(request: &ApprovalView, outside: Option<&str>, room: Room) -> Detail {
    detail_in(request, outside, room, Style::Listed)
}

/// The detail under the approval card's headline: the bare command (the purpose is the
/// headline already) and, when the risk tag does not already say "Outside the project", a last
/// row for the path outside it.
pub fn card_detail(
    request: &ApprovalView,
    outside: Option<&str>,
    room: Room,
    risk: Option<ApprovalRisk>,
) -> Detail {
    let style = if risk == Some(ApprovalRisk::OutsideProject) {
        Style::CardTagged
    } else {
        Style::Card
    };
    detail_in(request, outside, room, style)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// The inbox: `$ command`, the purpose under it, "Outside the project: <path>".
    Listed,
    /// The card: the command alone, "Outside the project: <path>".
    Card,
    /// The card, when its tag already says "Outside the project": just the path.
    CardTagged,
}

fn detail_in(request: &ApprovalView, outside: Option<&str>, room: Room, style: Style) -> Detail {
    // The warning gets its row first: it matters more than the purpose.
    let outside = outside.map(|path| match style {
        Style::CardTagged => short_path(path, room.cols.max(12)),
        _ => outside_line(path, room.cols),
    });
    let rows = room
        .rows
        .saturating_sub(usize::from(outside.is_some()))
        .max(1);
    let listed = style == Style::Listed;
    let (mut text, shortened) = if let ApprovalKind::Command {
        command: Some(command),
        purpose,
    } = &request.kind
    {
        // The purpose gets one row, unless the room is so small the command needs it.
        let purpose = purpose.as_deref().filter(|_| listed && rows >= 3);
        let command_rows = rows - usize::from(purpose.is_some());
        let full = if listed {
            format!("$ {command}")
        } else {
            command.clone()
        };
        let mut text = fit_rows(&full, room.cols, command_rows);
        // Only the command counts as shortened: the purpose is Claude's comment, not what runs.
        let shortened = text != full;
        if let Some(purpose) = purpose {
            text.push('\n');
            text.push_str(&fit_rows(&format!("# {purpose}"), room.cols, 1));
        }
        (text, shortened)
    } else {
        let specific = match &request.kind {
            ApprovalKind::FileChange { path } => path.clone(),
            ApprovalKind::Command { .. } | ApprovalKind::OtherTool => None,
        };
        let full = specific
            .unwrap_or_else(|| serde_json::to_string(&request.tool.arguments).unwrap_or_default());
        // Not a command: whether this was cut never decides what "Allow in this thread"
        // remembers.
        let text = fit_rows(&cap(&full, DETAIL_LINES, DETAIL_CHARS), room.cols, rows);
        (text, false)
    };
    if let Some(outside) = outside {
        text.push('\n');
        text.push_str(&outside);
    }
    Detail { text, shortened }
}

/// The icon in the card's tile: what kind of action this is.
pub fn glyph(request: &ApprovalView, risk: Option<ApprovalRisk>) -> IconName {
    if risk == Some(ApprovalRisk::DeletesFiles) {
        return IconName::Trash;
    }
    match &request.kind {
        ApprovalKind::FileChange { .. } => IconName::FilePen,
        ApprovalKind::Command { .. } => IconName::Terminal,
        ApprovalKind::OtherTool => {
            let name = request.tool.name.to_ascii_lowercase();
            if name.starts_with("mcp__") {
                IconName::Plug
            } else if name.contains("web") {
                IconName::Globe
            } else {
                IconName::Shield
            }
        }
    }
}

/// The path the CLI says this call touches, if it lies outside `project`. This reads the disk
/// (to resolve symlinks), so the chat view calls it once per request, not on every frame.
pub fn outside_path(request: &ApprovalView, project: &Path) -> Option<String> {
    let path = request.affected_path.as_deref()?;
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

/// The quiet row under the dock that says what "Allow in this thread" will do, before the click:
/// `before`, then `code` in the mono font (the rule that is remembered), then `after`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    pub before: String,
    pub code: Option<String>,
    pub after: String,
    /// The remembered rule could not be shown in full, so "Allow in this thread" must be sent as
    /// allow once: nothing is remembered that the user could not read.
    pub once: bool,
}

/// The scope row for the adapter's remember offer.
/// `command_shortened` is [`Detail::shortened`]. The remembered rule is cut in its middle so
/// the row stays one line at about `cols` columns of the detail's text.
pub fn scope_row(scope: &RememberScope, command_shortened: bool, cols: usize) -> ScopeRow {
    // `cut`: the answer itself must be downgraded here (core already sends allow once for the
    // `OnceOnly` cases, so they need no help).
    let once = |why: &str, cut: bool| ScopeRow {
        before: format!("Allow in this thread works once here{why}"),
        code: None,
        after: String::new(),
        once: cut,
    };
    let (rules, meaning, shown_in_detail) = match scope {
        RememberScope::OnceOnly(OnceReason::FileChanges) => {
            return once(" — no safe rule for edits", false);
        }
        RememberScope::OnceOnly(OnceReason::NoSafeScope) => return once("", false),
        RememberScope::Rules {
            labels,
            meaning,
            shown_in_detail,
        } => (labels, meaning, *shown_in_detail),
    };
    // The thread keeps the rule (core saves it and gives it to every later `claude` of this
    // thread), so it lasts for the thread, not just for this claude process.
    // In a rule, `*` matches more than this one call, so say that in words. For Bash it stands
    // for any arguments; for other tools it is a path or domain pattern.
    let after = match meaning {
        RuleMeaning::Arguments => " (any arguments)",
        RuleMeaning::Pattern => " (a pattern)",
        RuleMeaning::Exact | RuleMeaning::OneTool => "",
    };
    const BEFORE: &str = "Allow in this thread remembers ";
    let room = cols.saturating_sub(BEFORE.len() + after.len()).max(12);
    // One line: a multi-line command's line breaks are shown as ↵.
    let full = rules.join(", ").replace('\n', " ↵ ");
    let code = middle(&full, room);
    // Where can the user read the whole rule? When it is exactly the Bash command, the detail
    // above shows it (this row only repeats it, cut), so what counts is whether the detail cut
    // it. Any other rule (a CLI suggestion such as `npm test *` or `domain:example.com`) is
    // shown only here, so what counts is whether this row cut it.
    let rule_is_command = shown_in_detail;
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

    fn request(tool_name: &str, input: serde_json::Value) -> ApprovalView {
        let summary = input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let label = tool_name
            .strip_prefix("mcp__")
            .map(|s| s.replace("__", " · "))
            .unwrap_or_else(|| tool_name.to_string());
        let (kind, question) = match tool_name {
            "Bash" => (
                ApprovalKind::Command {
                    command: input
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    purpose: input
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                },
                "Allow Bash to run this command?".to_string(),
            ),
            "Edit" => (
                ApprovalKind::FileChange {
                    path: Some(summary.clone()),
                },
                format!("Allow Claude to edit {summary}?"),
            ),
            _ => (
                ApprovalKind::OtherTool,
                format!("Allow Claude to use {label}?"),
            ),
        };
        ApprovalView {
            tool: newjen_core::agent::ToolDescriptor {
                id: "t1".to_string(),
                name: tool_name.to_string(),
                display_name: label,
                summary,
                arguments: input,
            },
            kind,
            question,
            affected_path: None,
            remember: RememberScope::OnceOnly(OnceReason::NoSafeScope),
        }
    }
    fn purpose(request: &mut ApprovalView, text: String) {
        if let ApprovalKind::Command { purpose, .. } = &mut request.kind {
            *purpose = Some(text);
        }
    }
    fn rules(labels: Vec<String>, meaning: RuleMeaning, shown_in_detail: bool) -> RememberScope {
        RememberScope::Rules {
            labels,
            meaning,
            shown_in_detail,
        }
    }

    #[test]
    fn bash_shows_the_command() {
        let ask = request("Bash", json!({"command": "ls -la", "description": "List"}));
        assert_eq!(title(&ask), "Allow Bash to run this command?");
        assert_eq!(detail(&ask, None, ROOMY).text, "$ ls -la\n# List");

        let mut described = request("Bash", json!({"command": "touch a.txt"}));
        purpose(&mut described, "Create an empty file".to_string());
        assert_eq!(
            detail(&described, None, ROOMY).text,
            "$ touch a.txt\n# Create an empty file"
        );
    }

    #[test]
    fn the_card_shows_the_bare_command_without_its_purpose() {
        let ask = request(
            "Bash",
            json!({"command": "rm a.rs b.rs", "description": "Delete"}),
        );
        assert_eq!(card_detail(&ask, None, ROOMY, None).text, "rm a.rs b.rs");
        let mut ask = request("Bash", json!({"command": "touch new.txt"}));
        ask.affected_path = Some("/elsewhere/new.txt".to_string());
        assert_eq!(
            card_detail(&ask, Some("/elsewhere/new.txt"), ROOMY, None).text,
            "touch new.txt\nOutside the project: /elsewhere/new.txt"
        );
        // The tag already says "Outside the project", so the row keeps only the path.
        assert_eq!(
            card_detail(
                &ask,
                Some("/elsewhere/new.txt"),
                ROOMY,
                Some(ApprovalRisk::OutsideProject)
            )
            .text,
            "touch new.txt\n/elsewhere/new.txt"
        );
        let long = (1..=40)
            .map(|n| format!("echo {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let cut = card_detail(
            &request("Bash", json!({ "command": long })),
            None,
            ROOMY,
            None,
        );
        assert!(
            cut.shortened && cut.text.ends_with("echo 40"),
            "{}",
            cut.text
        );
    }

    #[test]
    fn the_tile_icon_names_the_kind_of_action() {
        let bash = request("Bash", json!({"command": "ls"}));
        assert_eq!(glyph(&bash, None), IconName::Terminal);
        assert_eq!(
            glyph(&bash, Some(ApprovalRisk::DeletesFiles)),
            IconName::Trash
        );
        assert_eq!(
            glyph(&request("Edit", json!({"file_path": "a.rs"})), None),
            IconName::FilePen
        );
        assert_eq!(
            glyph(&request("mcp__linear__search", json!({})), None),
            IconName::Plug
        );
        assert_eq!(
            glyph(&request("WebFetch", json!({})), None),
            IconName::Globe
        );
    }

    #[test]
    fn edits_name_the_file_relative_to_the_project() {
        let mut ask = request("Edit", json!({"file_path": "/work/app/src/main.rs"}));
        ask.kind = ApprovalKind::FileChange {
            path: Some("src/main.rs".to_string()),
        };
        ask.question = "Allow Claude to edit src/main.rs?".to_string();
        assert_eq!(title(&ask), "Allow Claude to edit src/main.rs?");
        assert_eq!(detail(&ask, None, ROOMY).text, "src/main.rs");
    }

    #[test]
    fn other_tools_show_compact_json() {
        let ask = request("mcp__linear__search", json!({"label": "billing"}));
        assert_eq!(title(&ask), "Allow Claude to use linear · search?");
        assert_eq!(detail(&ask, None, ROOMY).text, r#"{"label":"billing"}"#);

        // The same name as the tool's card, even when the CLI calls it something else.
        let mut named = request("WebFetch", json!({"url": "https://example.com"}));
        named.tool.display_name = "Fetch".to_string();
        assert_eq!(title(&named), "Allow Claude to use WebFetch?");
    }

    #[test]
    fn long_commands_keep_their_first_and_last_lines() {
        let command = (1..=20)
            .map(|n| format!("echo {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let ask = request("Bash", json!({ "command": command }));
        assert_eq!(
            detail(&ask, None, ROOMY).text,
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
        purpose(
            &mut ask,
            "Rebuild everything from scratch, slowly".to_string(),
        );
        let room = Room { cols: 75, rows: 6 };
        let shown = detail(&ask, None, room).text;
        assert!(text_rows(&shown, room.cols) <= room.rows, "{shown}");
        assert!(shown.starts_with("$ echo line 1 "), "{shown}");
        // The line that does the damage and the purpose stay visible.
        assert!(shown.contains("\nrm -rf build\n# Rebuild"), "{shown}");
    }

    #[test]
    fn a_shortened_command_is_flagged_so_always_allow_acts_once() {
        let ask = request("Bash", json!({"command": "touch notes.md"}));
        assert!(!detail(&ask, None, ROOMY).shortened);
        let long = (1..=40)
            .map(|n| format!("echo {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let cut = detail(&request("Bash", json!({ "command": long })), None, ROOMY);
        assert!(cut.shortened, "{}", cut.text);
        // A long purpose is cut too, but it is only Claude's comment: not shortened.
        let mut chatty = request("Bash", json!({"command": "ls"}));
        purpose(&mut chatty, "word ".repeat(400));
        assert!(!detail(&chatty, None, Room { cols: 40, rows: 3 }).shortened);

        let scope = rules(vec!["touch notes.md".to_string()], RuleMeaning::Exact, true);
        let row = scope_row(&scope, true, 80);
        assert_eq!(
            row.before,
            "Allow in this thread works once here — the command was shortened to fit"
        );
        assert!(row.once);
    }

    #[test]
    fn cut_arguments_do_not_matter_when_the_rule_does_not_use_them() {
        // An MCP call with huge arguments: the detail is cut, but the rule is just the tool.
        let ask = request("mcp__x__y", json!({"text": "word ".repeat(400)}));
        let shown = detail(&ask, None, Room { cols: 40, rows: 3 });
        assert!(!shown.shortened, "{}", shown.text);
        let scope = rules(vec!["x · y".to_string()], RuleMeaning::OneTool, false);
        let row = scope_row(&scope, shown.shortened, 80);
        assert!(!row.once);
        assert_eq!(row.code.as_deref(), Some("x · y"));

        // A CLI rule too long for its row is cut there, the only place it is shown.
        let long = rules(
            vec![format!("domain:{}.com", "a".repeat(200))],
            RuleMeaning::Exact,
            false,
        );
        let row = scope_row(&long, false, 80);
        assert_eq!(
            row.before,
            "Allow in this thread works once here — the rule was shortened to fit"
        );
        assert!(row.once);

        // A CLI rule for Bash that is not the command itself: also judged by this row.
        let short_rule = rules(
            vec!["npm test *".to_string()],
            RuleMeaning::Arguments,
            false,
        );
        assert!(!scope_row(&short_rule, true, 80).once);
    }

    #[test]
    fn the_scope_row_says_what_always_allow_remembers() {
        let text = |row: ScopeRow| {
            let code = row.code.map(|code| format!("`{code}`")).unwrap_or_default();
            format!("{}{code}{}", row.before, row.after)
        };
        let exact = rules(vec!["touch notes.md".to_string()], RuleMeaning::Exact, true);
        assert_eq!(
            text(scope_row(&exact, false, 80)),
            "Allow in this thread remembers `touch notes.md`"
        );
        let wildcard = rules(
            vec!["npm test *".to_string()],
            RuleMeaning::Arguments,
            false,
        );
        assert_eq!(
            text(scope_row(&wildcard, false, 80)),
            "Allow in this thread remembers `npm test *` (any arguments)"
        );
        // Outside Bash, `*` is a path or domain pattern, not "any arguments".
        let domains = rules(
            vec!["domain:*.example.com".to_string()],
            RuleMeaning::Pattern,
            false,
        );
        assert_eq!(
            text(scope_row(&domains, false, 80)),
            "Allow in this thread remembers `domain:*.example.com` (a pattern)"
        );
        assert_eq!(
            text(scope_row(
                &RememberScope::OnceOnly(OnceReason::FileChanges),
                false,
                80
            )),
            "Allow in this thread works once here — no safe rule for edits"
        );
        assert_eq!(
            text(scope_row(
                &RememberScope::OnceOnly(OnceReason::NoSafeScope),
                false,
                80
            )),
            "Allow in this thread works once here"
        );
        // A long command is cut in its middle to keep the row to one line; the detail above
        // shows it in full, so "Allow in this thread" still remembers it.
        let command = format!("echo {}done", "x".repeat(200));
        let long = rules(vec![command.clone()], RuleMeaning::Exact, true);
        let row = scope_row(&long, false, 80);
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
        ask.affected_path = Some(raw.join("new.txt").display().to_string());
        assert_eq!(outside_path(&ask, &project), None);

        ask.affected_path = Some("/elsewhere/notes/new.txt".to_string());
        let outside = outside_path(&ask, &project);
        assert_eq!(outside.as_deref(), Some("/elsewhere/notes/new.txt"));
        // `detail` itself only formats the path it is given: no disk access while drawing.
        assert_eq!(
            detail(&ask, outside.as_deref(), ROOMY).text,
            "$ touch new.txt\nOutside the project: /elsewhere/notes/new.txt"
        );
        assert_eq!(detail(&ask, None, ROOMY).text, "$ touch new.txt");
        let _ = std::fs::remove_dir_all(&raw);
    }
}
