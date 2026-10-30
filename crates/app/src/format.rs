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

/// How long something has been going, in whole units for a line that ticks once a second:
/// "12s", "1m 12s", "3h 12m".
pub fn elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    }
}

/// When something happened, from `now`, as the thread list says it: "Just now", "4m ago",
/// "2h ago", "Yesterday", "3 days ago", "3 weeks ago", "5 months ago", "2 years ago". Both
/// times are Unix milliseconds. A time in the future (a clock that moved back) reads as
/// "Just now".
pub fn relative_time(then_ms: i64, now_ms: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let secs = (now_ms - then_ms).max(0) / 1000;
    let plural = |count: i64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    if secs < MINUTE {
        "Just now".to_string()
    } else if secs < HOUR {
        format!("{}m ago", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h ago", secs / HOUR)
    } else if secs < 2 * DAY {
        "Yesterday".to_string()
    } else if secs < 14 * DAY {
        plural(secs / DAY, "day")
    } else if secs < 60 * DAY {
        plural(secs / (7 * DAY), "week")
    } else if secs < 365 * DAY {
        plural(secs / (30 * DAY), "month")
    } else {
        plural(secs / (365 * DAY), "year")
    }
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
pub fn middle(line: &str, max_chars: usize) -> String {
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

/// How many columns `ch` takes in a monospace font: CJK characters and emoji are drawn twice
/// as wide as Latin letters.
fn columns(ch: char) -> usize {
    match u32::from(ch) {
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Whether GPUI keeps `ch` attached to the characters around it when it wraps text (a copy of
/// GPUI's private `LineWrapper::is_word_char`). Anything else, such as `/` in a path, is a
/// place a line may break.
fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(ch, '\u{00C0}'..='\u{024F}' | '\u{0400}'..='\u{04FF}')
        || matches!(ch, '\u{1E00}'..='\u{1EFF}' | '\u{0300}'..='\u{036F}')
        || matches!(ch, '\u{0980}'..='\u{09FF}')
        // Punctuation that stays with its word, like `a-b`, `I'm` or `plz!`.
        || "-_.'’‘$%@#^~,=:;!)]}\"”»…⋯".contains(ch)
        || matches!(ch, '\u{202F}' | '\u{00A0}' | '\u{2011}')
}

/// How many rows `line` takes when GPUI wraps it at `cols` monospace columns. This follows
/// GPUI's own rule (`LineLayout::compute_wrap_boundaries`): a row may break before a word
/// that follows a space, or before any character that is not part of a word (like `/`); when
/// a row overflows it breaks at the last such place, or right there if it has none.
pub fn wrapped_rows(line: &str, cols: usize) -> usize {
    let cols = cols.max(1);
    let mut rows = 1;
    // Columns are counted from the start of the line. `row_start` is where the current row
    // begins and `x` where the current character begins.
    let mut row_start = 0;
    let mut x = 0;
    let mut break_at: Option<usize> = None;
    let mut seen_text = false;
    let mut prev = '\0';
    for ch in line.chars() {
        let can_break_here = if is_word_char(ch) {
            prev == ' ' && seen_text
        } else {
            ch != ' ' && seen_text
        };
        if can_break_here {
            break_at = Some(x);
        }
        if ch != ' ' {
            seen_text = true;
        }
        let end = x + columns(ch);
        if end - row_start > cols && x > row_start {
            row_start = break_at.take().unwrap_or(x);
            rows += 1;
        }
        prev = ch;
        x = end;
    }
    rows
}

/// Total rows of every line of `text` at `cols` columns.
pub fn text_rows(text: &str, cols: usize) -> usize {
    text.lines().map(|line| wrapped_rows(line, cols)).sum()
}

/// Shortens `text` so it wraps to at most `max_rows` rows at `cols` columns. Long lines are
/// cut in their middle (to two rows each, then one); if there are still too many lines, the
/// middle lines become "… N more lines …", keeping a little more of the end than the start
/// (the last line of a shell command is often the one that does the work).
pub fn fit_rows(text: &str, cols: usize, max_rows: usize) -> String {
    let cols = cols.max(8);
    let max_rows = max_rows.max(1);
    if text_rows(text, cols) <= max_rows {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let two_rows: Vec<String> = lines.iter().map(|line| fit_line(line, cols, 2)).collect();
    if two_rows
        .iter()
        .map(|line| wrapped_rows(line, cols))
        .sum::<usize>()
        <= max_rows
    {
        return two_rows.join("\n");
    }
    let one_row: Vec<String> = lines.iter().map(|line| fit_line(line, cols, 1)).collect();
    if one_row.len() <= max_rows {
        return one_row.join("\n");
    }
    // One row is kept for the "… N more lines …" marker.
    let head = (max_rows - 1) / 2;
    let tail = max_rows - 1 - head;
    let hidden = one_row.len() - head - tail;
    let mut kept: Vec<String> = one_row[..head].to_vec();
    kept.push(format!("… {hidden} more lines …"));
    kept.extend_from_slice(&one_row[one_row.len() - tail..]);
    kept.join("\n")
}

/// `line` cut in its middle until it wraps to at most `max_rows` rows at `cols` columns.
fn fit_line(line: &str, cols: usize, max_rows: usize) -> String {
    if wrapped_rows(line, cols) <= max_rows {
        return line.to_string();
    }
    // Start from a full budget of characters and trim until it wraps short enough (word
    // breaks and wide characters can make a cut line need an extra row). A cut line of three
    // characters (" … ") always fits, so this ends.
    let step = (cols / 8).max(1);
    let mut keep = (cols * max_rows).min(line.chars().count());
    loop {
        let short = middle(line, keep);
        if keep <= 3 || wrapped_rows(&short, cols) <= max_rows {
            return short;
        }
        keep = keep.saturating_sub(step).max(3);
    }
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
    fn elapsed_reads_like_the_thread_list() {
        assert_eq!(elapsed(0), "0s");
        assert_eq!(elapsed(12), "12s");
        assert_eq!(elapsed(72), "1m 12s");
        assert_eq!(elapsed(3_599), "59m 59s");
        assert_eq!(elapsed(3 * 3600 + 12 * 60 + 40), "3h 12m");
    }

    #[test]
    fn relative_time_reads_like_the_thread_list() {
        let now = 1_000_000_000_000;
        let ago = |secs: i64| relative_time(now - secs * 1000, now);
        assert_eq!(ago(0), "Just now");
        assert_eq!(ago(59), "Just now");
        assert_eq!(ago(60), "1m ago");
        assert_eq!(ago(4 * 60 + 30), "4m ago");
        assert_eq!(ago(2 * 3600), "2h ago");
        assert_eq!(ago(30 * 3600), "Yesterday");
        assert_eq!(ago(2 * 86_400), "2 days ago");
        assert_eq!(ago(9 * 86_400), "9 days ago");
        assert_eq!(ago(21 * 86_400), "3 weeks ago");
        assert_eq!(ago(90 * 86_400), "3 months ago");
        assert_eq!(ago(365 * 86_400), "1 year ago");
        assert_eq!(ago(800 * 86_400), "2 years ago");
        // A clock that moved back never shows a negative time.
        assert_eq!(relative_time(now + 5_000, now), "Just now");
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

    #[test]
    fn wrapped_rows_breaks_between_words() {
        assert_eq!(wrapped_rows("", 10), 1);
        assert_eq!(wrapped_rows("0123456789", 10), 1);
        assert_eq!(wrapped_rows("0123456789a", 10), 2);
        // "aaaa bbbb" fits; "cccc" moves to the next row instead of splitting.
        assert_eq!(wrapped_rows("aaaa bbbb cccc", 10), 2);
        // A word wider than a row is split inside.
        assert_eq!(wrapped_rows("a 0123456789012345678901", 10), 4);
        // A path breaks before a `/`, which can waste room: 39 characters need three rows
        // of 20, not two.
        let path = format!("{}/{}", "a".repeat(15), "b".repeat(23));
        assert_eq!(wrapped_rows(&path, 20), 3);
        // CJK characters take two columns each.
        assert_eq!(wrapped_rows("日本語日本語", 10), 2);
    }

    #[test]
    fn fit_rows_keeps_long_text_inside_its_rows_and_keeps_the_end() {
        // 20 lines of 200 characters: 60 rows at 77 columns, far over a 15-row budget.
        let text = (1..=20)
            .map(|n| format!("line{n:02} {}", "x".repeat(193)))
            .collect::<Vec<_>>()
            .join("\n");
        let fitted = fit_rows(&text, 77, 15);
        assert!(text_rows(&fitted, 77) <= 15, "{fitted}");
        let lines: Vec<&str> = fitted.lines().collect();
        assert!(lines[0].starts_with("line01 "));
        assert!(lines.last().unwrap().starts_with("line20 "));
        assert!(fitted.contains("… 6 more lines …"), "{fitted}");

        // One huge line keeps both ends.
        let long = format!("start {} end", "y".repeat(5_000));
        let fitted = fit_rows(&long, 40, 3);
        assert!(text_rows(&fitted, 40) <= 3);
        assert!(fitted.starts_with("start ") && fitted.ends_with(" end"));

        // Text that already fits is untouched.
        assert_eq!(fit_rows("a\nb", 40, 2), "a\nb");
    }
}
