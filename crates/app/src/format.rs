//! Small text helpers for the UI. Kept free of GPUI so they are easy to unit-test.

use std::path::Path;

/// `path` with the home directory shown as `~`, e.g. `/Users/ana/Newjen` → `~/Newjen`.
/// Used for display only; never pass the result to a process.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(inside) = path.strip_prefix(home)
    {
        if inside.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", inside.display());
    }
    path.display().to_string()
}

/// A display path that fits in `max_chars`, cut in the middle so both ends stay readable:
/// `~/Developer/clients/acme/platform/services/billing` → `~/…/services/billing`.
/// It tries, in order: first folder + `…` + last two, `…` + last two, `…` + last one.
/// The last folders are what tell projects (and later, copies) apart, so they are kept.
pub fn short_path(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    let parts: Vec<&str> = label.split('/').filter(|part| !part.is_empty()).collect();
    // The first folder as written: `~`, `/private`, or `src`.
    let first = match (label.starts_with('/'), parts.first()) {
        (true, Some(first)) => format!("/{first}"),
        (false, Some(first)) => first.to_string(),
        (_, None) => String::new(),
    };
    let rest = &parts[1.min(parts.len())..];
    let last_two = (rest.len() >= 2).then(|| rest[rest.len() - 2..].join("/"));
    let candidates = [
        last_two
            .as_ref()
            .filter(|_| rest.len() > 2)
            .map(|end| format!("{first}/…/{end}")),
        last_two.as_ref().map(|end| format!("…/{end}")),
        rest.last().map(|last| format!("…/{last}")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if candidate.chars().count() <= max_chars {
            return candidate;
        }
    }
    // Even the last folder is too long: keep its end.
    let chars: Vec<char> = label.chars().collect();
    let keep = max_chars.saturating_sub(1).min(chars.len());
    let mut short = String::from("…");
    short.extend(&chars[chars.len() - keep..]);
    short
}

/// The caption under a finished turn, e.g. "Done in 4.2s · $0.0041", or "Stopped".
pub fn turn_caption(duration_ms: Option<u64>, cost_usd: Option<f64>, stopped: bool) -> String {
    if stopped {
        return "Stopped".to_string();
    }
    let mut caption = "Done".to_string();
    if let Some(ms) = duration_ms {
        caption.push_str(" in ");
        caption.push_str(&seconds(ms));
    }
    if let Some(cost) = cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0) {
        caption.push_str(&format!(" · ${cost:.4}"));
    }
    caption
}

/// "0.4s", "4.2s", "1m 5s", "1h 15m".
fn seconds(ms: u64) -> String {
    let tenths = (ms + 50) / 100;
    if tenths < 600 {
        return format!("{}.{}s", tenths / 10, tenths % 10);
    }
    let secs = (ms + 500) / 1000;
    if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        let mins = (secs + 30) / 60;
        format!("{}h {}m", mins / 60, mins % 60)
    }
}

/// Shortens `text` to at most `max_lines` lines and `max_chars` characters, saying how much was
/// cut. Long tool output would otherwise make the conversation slow to lay out and scroll.
pub fn cap(text: &str, max_lines: usize, max_chars: usize) -> String {
    let total_lines = text.lines().count();
    let mut kept = String::new();
    // Counted as we go: recounting `kept` for every line would be slow on big outputs.
    let mut kept_chars = 0;
    let mut kept_lines = 0;
    for line in text.lines().take(max_lines) {
        let line_chars = line.chars().count();
        if kept_chars + line_chars > max_chars {
            let room = max_chars.saturating_sub(kept_chars);
            if kept_lines > 0 {
                kept.push('\n');
            }
            kept.extend(line.chars().take(room));
            kept.push('…');
            kept_lines += 1;
            break;
        }
        if kept_lines > 0 {
            kept.push('\n');
        }
        kept.push_str(line);
        kept_chars += line_chars;
        kept_lines += 1;
    }
    let hidden = total_lines.saturating_sub(kept_lines);
    match hidden {
        0 => kept,
        1 => format!("{kept}\n… 1 more line"),
        _ => format!("{kept}\n… {hidden} more lines"),
    }
}

/// Like `cap`, but keeps the first `head` and last `tail` lines and cuts the middle. Used where
/// the end matters: the last line of a shell command is often the one that does the work, and
/// error output usually ends with its summary. Each kept line is also cut in its middle to
/// `max_line_chars`, so one huge line cannot hide its end either.
pub fn cap_ends(text: &str, head: usize, tail: usize, max_line_chars: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let shorten = |line: &str| middle(line, max_line_chars);
    if lines.len() <= head + tail + 1 {
        return lines
            .iter()
            .map(|line| shorten(line))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let hidden = lines.len() - head - tail;
    let mut kept: Vec<String> = lines[..head].iter().map(|line| shorten(line)).collect();
    kept.push(format!("… {hidden} more lines …"));
    kept.extend(lines[lines.len() - tail..].iter().map(|line| shorten(line)));
    kept.join("\n")
}

/// `line` cut to `max_chars` by replacing its middle with " … ".
fn middle(line: &str, max_chars: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max_chars {
        return line.to_string();
    }
    let room = max_chars.saturating_sub(3);
    let front = room.div_ceil(2);
    let back = room - front;
    let mut short: String = chars[..front].iter().collect();
    short.push_str(" … ");
    short.extend(&chars[chars.len() - back..]);
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_shortens_paths_in_home() {
        let home = Path::new("/Users/ana");
        assert_eq!(
            tilde(Path::new("/Users/ana/Newjen"), Some(home)),
            "~/Newjen"
        );
        assert_eq!(tilde(Path::new("/Users/ana"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/tmp/x"), Some(home)), "/tmp/x");
        assert_eq!(
            tilde(Path::new("/Users/anabel"), Some(home)),
            "/Users/anabel"
        );
        assert_eq!(tilde(Path::new("/tmp/x"), None), "/tmp/x");
    }

    #[test]
    fn short_path_cuts_the_middle() {
        assert_eq!(short_path("~/Newjen", 30), "~/Newjen");
        assert_eq!(
            short_path("~/a/b/c/services/billing", 20),
            "~/…/services/billing"
        );
        assert_eq!(
            short_path("/private/tmp/x/y/services/billing", 29),
            "/private/…/services/billing"
        );
        assert_eq!(
            short_path("/private/tmp/x/y/services/billing", 22),
            "…/services/billing"
        );
        assert_eq!(short_path("~/a/b/c/services/billing", 12), "…/billing");
        assert_eq!(short_path("~/a/b/averyveryverylongname", 10), "…ylongname");
        assert_eq!(
            short_path("~/a/b/averyveryverylongname", 10)
                .chars()
                .count(),
            10
        );
    }

    #[test]
    fn turn_caption_reads_like_the_mock() {
        assert_eq!(
            turn_caption(Some(4_200), Some(0.0041), false),
            "Done in 4.2s · $0.0041"
        );
        assert_eq!(turn_caption(Some(65_000), None, false), "Done in 1m 5s");
        assert_eq!(turn_caption(Some(4_523_417), None, false), "Done in 1h 15m");
        assert_eq!(turn_caption(None, None, false), "Done");
        assert_eq!(turn_caption(Some(1_000), Some(0.5), true), "Stopped");
    }

    #[test]
    fn cap_keeps_short_text_and_counts_what_it_cuts() {
        assert_eq!(cap("a\nb", 8, 100), "a\nb");
        assert_eq!(cap("a\nb\nc\nd", 2, 100), "a\nb\n… 2 more lines");
        assert_eq!(cap("a\nb\nc", 2, 100), "a\nb\n… 1 more line");
        assert_eq!(cap("abcdef", 8, 3), "abc…");
        assert_eq!(cap("ab\ncdef", 8, 4), "ab\ncd…");
        assert_eq!(cap("", 8, 100), "");
    }

    #[test]
    fn cap_ends_keeps_both_ends() {
        let text = (1..=10)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            cap_ends(&text, 2, 3, 100),
            "1\n2\n… 5 more lines …\n8\n9\n10"
        );
        // Cutting a single line would hide nothing, so it is shown.
        assert_eq!(cap_ends("1\n2\n3\n4", 2, 1, 100), "1\n2\n3\n4");
        assert_eq!(cap_ends("abcdefghij", 2, 2, 7), "ab … ij");
    }
}
