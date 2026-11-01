use crate::agent::{GrantId, GrantLifetime, GrantView, ToolDisposition};
use crate::transcript::{BACKGROUND_STOPPED_NOTE, EXPIRED_NOTE, STOPPED_TOOL_NOTE, ToolState};
/// Splits a grant (its CLI `rule`, such as `Bash(git add:*)`, and its `label`, such as
/// `git add:*`) into what the popover shows.
pub fn grant_view(rule: &str, label: &str) -> GrantView {
    // One line: a multi-line command's line breaks are shown as ↵, as the approval dock does.
    let shown = label.replace('\n', " ↵ ");
    let tool = rule.split('(').next().unwrap_or(rule);
    if !rule.contains('(') && tool.starts_with("mcp__") {
        return GrantView {
            id: GrantId(rule.to_string()),
            lifetime: GrantLifetime::Thread,
            tool: "MCP".to_string(),
            shown,
            note: Some("this tool only".to_string()),
        };
    }
    // In a Bash rule a trailing `:*` or ` *` means "any arguments"; another `*` is a pattern.
    let note = if tool == "Bash" {
        let prefix = label
            .strip_suffix(":*")
            .or_else(|| label.strip_suffix(" *"))
            .map(str::trim_end)
            .filter(|prefix| !prefix.is_empty() && !prefix.contains('*'));
        Some(match prefix {
            Some(prefix) => format!("{} with any arguments", prefix.replace('\n', " ↵ ")),
            None if label.contains('*') => "a pattern".to_string(),
            None => "exact command".to_string(),
        })
    } else {
        label.contains('*').then(|| "a pattern".to_string())
    };
    GrantView {
        id: GrantId(rule.to_string()),
        lifetime: GrantLifetime::Thread,
        tool: tool.to_string(),
        shown,
        note,
    }
}

pub fn failure_reason(title: &str, detail: &str) -> String {
    let first_line = detail.lines().map(str::trim).find(|line| !line.is_empty());
    match first_line {
        Some(line) if title == "Claude stopped" => line.to_string(),
        _ => title.to_string(),
    }
}
pub fn tool_disposition(state: ToolState, output: Option<&str>) -> ToolDisposition {
    match state {
        ToolState::Expired => ToolDisposition::Expired {
            note: format!("Expired · {EXPIRED_NOTE}"),
        },
        ToolState::Failed
            if let Some(note @ (STOPPED_TOOL_NOTE | BACKGROUND_STOPPED_NOTE)) = output =>
        {
            ToolDisposition::Stopped {
                note: note.to_string(),
            }
        }
        _ => ToolDisposition::Ordinary,
    }
}

#[test]
fn patterns_and_other_tools_are_named_honestly() {
    assert_eq!(
        test_parts("Bash(npm test *)", "npm test *").2.as_deref(),
        Some("npm test with any arguments")
    );
    assert_eq!(
        test_parts("Bash(rm *.log)", "rm *.log").2.as_deref(),
        Some("a pattern")
    );
    // A bare `*` allows anything: never "with any arguments" of nothing.
    assert_eq!(test_parts("Bash(*)", "*").2.as_deref(), Some("a pattern"));
    assert_eq!(
        test_parts("WebFetch(domain:*.example.com)", "domain:*.example.com"),
        (
            "WebFetch".to_string(),
            "domain:*.example.com".to_string(),
            Some("a pattern".to_string())
        )
    );
    assert_eq!(
        test_parts("WebFetch(domain:example.com)", "domain:example.com").2,
        None
    );
    // Escaped brackets in the CLI rule do not confuse the tool name.
    assert_eq!(
        test_parts(r"Bash(sed 's/\\\(a\\\)/b/' f)", r"sed 's/\(a\)/b/' f").0,
        "Bash"
    );
    // A multi-line command stays on one line.
    assert_eq!(
        test_parts("Bash(echo a\necho b)", "echo a\necho b").1,
        "echo a ↵ echo b"
    );
}
#[test]
fn grants_read_like_the_popover_mock() {
    assert_eq!(
        test_parts("Bash(touch one.txt)", "touch one.txt"),
        (
            "Bash".to_string(),
            "touch one.txt".to_string(),
            Some("exact command".to_string())
        )
    );
    assert_eq!(
        test_parts("Bash(git add:*)", "git add:*"),
        (
            "Bash".to_string(),
            "git add:*".to_string(),
            Some("git add with any arguments".to_string())
        )
    );
    assert_eq!(
        test_parts("mcp__pencil__get_style", "pencil · get_style"),
        (
            "MCP".to_string(),
            "pencil · get_style".to_string(),
            Some("this tool only".to_string())
        )
    );
}

#[cfg(test)]
fn test_parts(rule: &str, label: &str) -> (String, String, Option<String>) {
    let parts = grant_view(rule, label);
    (parts.tool, parts.shown, parts.note)
}

#[test]
fn a_failure_says_why_in_a_few_words() {
    assert_eq!(
        failure_reason("Claude exited", "The claude process exited with code 1."),
        "Claude exited"
    );
    assert_eq!(
        failure_reason(
            "Claude stopped",
            "\nNot logged in · Please run /login\nmore"
        ),
        "Not logged in · Please run /login"
    );
    assert_eq!(failure_reason("Claude stopped", ""), "Claude stopped");
}

impl Default for crate::agent::AgentPresentation {
    fn default() -> Self {
        Self {
            label: "Claude",
            executable_label: "Claude",
            working_placeholder: "Claude is working…",
            reply_placeholder: "Reply to Claude…",
            empty_title: "What should Claude work on?",
            grants_detail: "Claude runs these without asking, until you revoke them.",
            send_failed: "Could not reach Claude",
            answer_failed: "Could not send your answer to Claude",
            stop_failed: "Could not stop Claude",
            start_failed: "Could not start Claude",
            installation_hint: "Install Claude Code and run `claude` once in a terminal to log in, then try again. Set NEWJEN_CLAUDE to use another path.",
            unreachable_note: "Claude could not be reached.",
            send_detail_prefix: "Newjen could not send the message to the claude process",
            interrupt_title: "Claude didn't stop, so Newjen ended the session",
            interrupt_detail: "Claude did not confirm the stop request in time. Try again to continue the conversation.",
            exit_title: "Claude exited",
            exited_tool_note: "Claude exited before this finished.",
        }
    }
}

#[cfg(test)]
mod tests {
    /// Newjen runs Claude Code but must not wear its name: the UI labels say "Claude". Only
    /// the install hint names the product the user has to install.
    #[test]
    fn ui_labels_do_not_say_claude_code() {
        let p = crate::agent::AgentPresentation::default();
        for label in [
            p.label,
            p.executable_label,
            p.working_placeholder,
            p.reply_placeholder,
            p.empty_title,
            p.grants_detail,
            p.send_failed,
            p.answer_failed,
            p.stop_failed,
            p.start_failed,
            p.unreachable_note,
            p.send_detail_prefix,
            p.interrupt_title,
            p.interrupt_detail,
            p.exit_title,
            p.exited_tool_note,
        ] {
            assert!(!label.contains("Claude Code"), "{label}");
        }
        assert!(
            p.empty_caption("~/Newjen")
                .starts_with("Claude runs in ~/Newjen")
        );
    }
}
