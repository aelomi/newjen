//! Per-file change statistics for a thread: the exact ones from a jj diff (see
//! [`crate::landing::changed_files`]) and a live estimate from the agent's edit tool calls.

use std::collections::BTreeMap;
use std::io::{self, BufRead};
use std::path::Path;

use crate::edits::{EditProposal, FileChange};
use crate::transcript::{Item, ToolState};

const HEADER_LIMIT: usize = 8192;
const DIFF_SIZE_LIMIT: usize = 400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    pub files: Vec<FileStat>,
}

impl ChangeSummary {
    pub fn added(&self) -> usize {
        self.files.iter().map(|file| file.added).sum()
    }

    pub fn removed(&self) -> usize {
        self.files.iter().map(|file| file.removed).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// What the agent's finished edit tool calls add up to, in transcript order. It never touches
/// the folder, so it is safe mid-turn. It is an estimate: a `Write` over an existing file counts
/// every line as added, `replace_all` counts one occurrence, and edits by shell commands are
/// invisible. The jj result replaces it once the turn is snapshotted.
pub fn estimate_from_items(items: &[Item], root: &Path) -> ChangeSummary {
    let proposals: Vec<EditProposal> = items
        .iter()
        .filter_map(|item| match item {
            Item::Tool {
                name,
                input,
                state: ToolState::Done,
                ..
            } => match name.as_str() {
                "Write" | "Edit" | "MultiEdit" => {
                    crate::providers::claude::edit_proposal(name, input, root)
                }
                "File change" => {
                    let tool = crate::agent::ToolDescriptor {
                        id: String::new(),
                        name: name.clone(),
                        display_name: String::new(),
                        summary: String::new(),
                        arguments: input.clone(),
                    };
                    Some(crate::providers::codex::edit_proposal(&tool, root))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    estimate(&proposals, root)
}

pub fn estimate<'a>(
    edits: impl IntoIterator<Item = &'a EditProposal>,
    root: &Path,
) -> ChangeSummary {
    let mut merged: BTreeMap<String, FileStat> = BTreeMap::new();
    for edit in edits {
        for stat in proposal_stats(edit, root) {
            match merged.get_mut(&stat.path) {
                Some(known) => merge(known, stat),
                None => {
                    merged.insert(stat.path.clone(), stat);
                }
            }
        }
    }
    ChangeSummary {
        files: merged.into_values().collect(),
    }
}

fn merge(known: &mut FileStat, next: FileStat) {
    known.added += next.added;
    known.removed += next.removed;
    known.binary |= next.binary;
    known.kind = match (known.kind, next.kind) {
        (ChangeKind::Added, ChangeKind::Modified | ChangeKind::Added) => ChangeKind::Added,
        (_, ChangeKind::Deleted) => ChangeKind::Deleted,
        (ChangeKind::Deleted, kind) => kind,
        (kind, ChangeKind::Added | ChangeKind::Modified) => kind,
        (_, kind) => kind,
    };
    if next.old_path.is_some() {
        known.old_path = next.old_path;
    }
}

fn proposal_stats(edit: &EditProposal, root: &Path) -> Vec<FileStat> {
    let single = |kind, added, removed| FileStat {
        path: relative(&edit.path.to_string_lossy(), root),
        old_path: None,
        kind,
        added,
        removed,
        binary: false,
    };
    match &edit.change {
        FileChange::Write(content) => {
            vec![single(ChangeKind::Added, content.lines().count(), 0)]
        }
        FileChange::Replace(replacements) => {
            let (mut added, mut removed) = (0, 0);
            for replacement in replacements {
                let (a, r) = line_delta(&replacement.old, &replacement.new);
                added += a;
                removed += r;
            }
            vec![single(ChangeKind::Modified, added, removed)]
        }
        FileChange::Patch(patch) => {
            let mut counter = DiffCounter::default();
            for line in patch.lines() {
                counter.line(line.as_bytes());
            }
            counter
                .finish()
                .files
                .into_iter()
                .map(|mut stat| {
                    stat.path = relative(&stat.path, root);
                    stat.old_path = stat.old_path.map(|path| relative(&path, root));
                    stat
                })
                .collect()
        }
        FileChange::Unavailable => Vec::new(),
    }
}

fn relative(path: &str, root: &Path) -> String {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    for prefix in [root, root.trim_start_matches('/')] {
        if prefix.is_empty() {
            continue;
        }
        if let Some(rest) = path.strip_prefix(prefix)
            && let Some(rest) = rest.strip_prefix('/')
        {
            return rest.to_string();
        }
    }
    path.to_string()
}

fn line_delta(old: &str, new: &str) -> (usize, usize) {
    use similar::DiffOp;
    if old.len() + new.len() > DIFF_SIZE_LIMIT {
        return (new.lines().count(), old.lines().count());
    }
    let diff = similar::TextDiff::configure()
        .deadline(std::time::Instant::now() + std::time::Duration::from_millis(200))
        .diff_lines(old, new);
    let (mut added, mut removed) = (0, 0);
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_len, .. } => removed += old_len,
            DiffOp::Insert { new_len, .. } => added += new_len,
            DiffOp::Replace {
                old_len, new_len, ..
            } => {
                removed += old_len;
                added += new_len;
            }
        }
    }
    (added, removed)
}

/// Counts lines per file in a git-style diff, one line at a time, without keeping the diff.
#[derive(Default)]
pub(crate) struct DiffCounter {
    files: Vec<FileStat>,
    current: Option<FileStat>,
    path_rank: u8,
    in_hunk: bool,
}

impl DiffCounter {
    pub(crate) fn line(&mut self, line: &[u8]) {
        if self.in_hunk && !line.starts_with(b"diff --git ") {
            match line.first() {
                Some(b'+') => {
                    if let Some(file) = &mut self.current {
                        file.added += 1;
                    }
                }
                Some(b'-') => {
                    if let Some(file) = &mut self.current {
                        file.removed += 1;
                    }
                }
                _ => {}
            }
            return;
        }
        let line = String::from_utf8_lossy(&line[..line.len().min(HEADER_LIMIT)]);
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = line.strip_prefix("diff --git ") {
            self.flush();
            self.in_hunk = false;
            self.path_rank = 0;
            self.current = Some(FileStat {
                path: header_path(rest),
                old_path: None,
                kind: ChangeKind::Modified,
                added: 0,
                removed: 0,
                binary: false,
            });
            return;
        }
        let Some(file) = &mut self.current else {
            return;
        };
        if line.starts_with("@@") {
            self.in_hunk = true;
        } else if line == "new file mode" || line.starts_with("new file mode ") {
            file.kind = ChangeKind::Added;
        } else if line.starts_with("deleted file mode ") {
            file.kind = ChangeKind::Deleted;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.kind = ChangeKind::Renamed;
            file.old_path = Some(clean(from));
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.kind = ChangeKind::Renamed;
            file.path = clean(to);
            self.path_rank = 3;
        } else if let Some(to) = line.strip_prefix("copy to ") {
            file.kind = ChangeKind::Added;
            file.path = clean(to);
            self.path_rank = 3;
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            file.binary = true;
        } else if let Some(old) = line.strip_prefix("--- ") {
            if old.starts_with("/dev/null") {
                file.kind = ChangeKind::Added;
            } else if self.path_rank < 1 {
                file.path = clean(old);
                self.path_rank = 1;
            }
        } else if let Some(new) = line.strip_prefix("+++ ") {
            if new.starts_with("/dev/null") {
                file.kind = ChangeKind::Deleted;
            } else if self.path_rank < 2 {
                file.path = clean(new);
                self.path_rank = 2;
            }
        }
    }

    fn flush(&mut self) {
        if let Some(file) = self.current.take()
            && !file.path.is_empty()
        {
            self.files.push(file);
        }
    }

    pub(crate) fn finish(mut self) -> ChangeSummary {
        self.flush();
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        ChangeSummary { files: self.files }
    }
}

pub(crate) fn count_git_diff(mut reader: impl BufRead) -> io::Result<ChangeSummary> {
    let mut counter = DiffCounter::default();
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let (take, found) = match chunk.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (chunk.len(), false),
        };
        let content = if found { take - 1 } else { take };
        let room = HEADER_LIMIT.saturating_sub(line.len());
        line.extend_from_slice(&chunk[..content.min(room)]);
        reader.consume(take);
        if found {
            counter.line(&line);
            line.clear();
        }
    }
    if !line.is_empty() {
        counter.line(&line);
    }
    Ok(counter.finish())
}

fn clean(token: &str) -> String {
    let token = token.trim_end_matches(['\r', '\n']);
    let token = if token.starts_with('"') {
        serde_json::from_str::<String>(token).unwrap_or_else(|_| token.to_string())
    } else {
        token.to_string()
    };
    for prefix in ["a/", "b/"] {
        if let Some(rest) = token.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    token
}

/// The path from `diff --git a/P b/P`, for entries that carry no other path line (an empty
/// new file, a binary file, a mode change).
fn header_path(rest: &str) -> String {
    if rest.starts_with('"') {
        let mut tokens = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let first = tokens.next().and_then(Result::ok);
        let second = tokens.next().and_then(Result::ok);
        return second
            .or(first)
            .map(|path| clean(&path))
            .unwrap_or_default();
    }
    let length = rest.len().saturating_sub(5) / 2;
    if rest.len() == 2 * length + 5
        && rest.is_char_boundary(2 + length)
        && rest[..2 + length].starts_with("a/")
        && rest[2 + length..].starts_with(" b/")
    {
        return rest[2..2 + length].to_string();
    }
    rest.rsplit_once(" b/")
        .map(|(_, path)| path.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edits::Replacement;
    use std::path::PathBuf;

    fn replace(path: &str, old: &str, new: &str) -> EditProposal {
        EditProposal {
            path: PathBuf::from(path),
            change: FileChange::Replace(vec![Replacement {
                old: old.into(),
                new: new.into(),
                all: false,
            }]),
        }
    }

    fn write(path: &str, content: &str) -> EditProposal {
        EditProposal {
            path: PathBuf::from(path),
            change: FileChange::Write(content.into()),
        }
    }

    #[test]
    fn a_replacement_counts_only_the_lines_that_changed() {
        let summary = estimate(
            &[replace(
                "/p/a.rs",
                "keep\nold\nkeep\n",
                "keep\nnew one\nnew two\nkeep\n",
            )],
            Path::new("/p"),
        );
        assert_eq!(summary.files.len(), 1);
        let file = &summary.files[0];
        assert_eq!(
            (file.path.as_str(), file.added, file.removed),
            ("a.rs", 2, 1)
        );
        assert_eq!(file.kind, ChangeKind::Modified);
    }

    #[test]
    fn a_write_counts_every_line_added_and_a_deletion_only_removes() {
        let summary = estimate(
            &[
                write("/p/new.txt", "one\ntwo\nthree"),
                replace("/p/b.rs", "gone\nalso gone\n", ""),
            ],
            Path::new("/p"),
        );
        let new = &summary.files[1];
        assert_eq!(
            (new.path.as_str(), new.kind, new.added, new.removed),
            ("new.txt", ChangeKind::Added, 3, 0)
        );
        let b = &summary.files[0];
        assert_eq!((b.added, b.removed), (0, 2));
        assert_eq!((summary.added(), summary.removed()), (3, 2));
    }

    #[test]
    fn edits_to_one_file_add_up_and_sort_by_path() {
        let summary = estimate(
            &[
                write("/p/z.txt", "a\nb\n"),
                replace("/p/a.txt", "x\n", "y\n"),
                replace("/p/z.txt", "a\n", "c\nd\n"),
            ],
            Path::new("/p"),
        );
        let paths: Vec<_> = summary.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "z.txt"]);
        let z = &summary.files[1];
        assert_eq!((z.kind, z.added, z.removed), (ChangeKind::Added, 4, 1));
    }

    #[test]
    fn a_path_outside_the_root_stays_as_given_and_unavailable_edits_are_skipped() {
        let summary = estimate(
            &[
                write("/elsewhere/x.txt", "x\n"),
                EditProposal {
                    path: PathBuf::from("/p/n.ipynb"),
                    change: FileChange::Unavailable,
                },
            ],
            Path::new("/p"),
        );
        assert_eq!(summary.files.len(), 1);
        assert_eq!(summary.files[0].path, "/elsewhere/x.txt");
    }

    #[test]
    fn a_provider_patch_gives_one_stat_per_file_with_its_kind() {
        let patch = "diff --git \"a/p/src/a.rs\" \"b/p/src/a.rs\"\n--- \"a/p/src/a.rs\"\n+++ \"b/p/src/a.rs\"\n@@ -1,2 +1,3 @@\n-old\n+new\n+newer\n keep\n\\ No newline at end of file\ndiff --git \"a/p/src/n.rs\" \"b/p/src/n.rs\"\n--- /dev/null\n+++ \"b/p/src/n.rs\"\n@@ -0,0 +1,2 @@\n+1\n+2\ndiff --git \"a/p/src/d.rs\" \"b/p/src/d.rs\"\n--- \"a/p/src/d.rs\"\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-bye\n";
        let summary = estimate(
            &[EditProposal {
                path: PathBuf::from("/p/src/a.rs"),
                change: FileChange::Patch(patch.into()),
            }],
            Path::new("/p"),
        );
        let got: Vec<_> = summary
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.kind, f.added, f.removed))
            .collect();
        assert_eq!(
            got,
            [
                ("src/a.rs", ChangeKind::Modified, 2, 1),
                ("src/d.rs", ChangeKind::Deleted, 0, 1),
                ("src/n.rs", ChangeKind::Added, 2, 0),
            ]
        );
    }

    fn items(tools: &[(&str, serde_json::Value, ToolState)]) -> Vec<Item> {
        tools
            .iter()
            .enumerate()
            .map(|(id, (name, input, state))| Item::Tool {
                id: id as u64,
                tool_use_id: format!("t{id}"),
                name: (*name).into(),
                display_name: (*name).into(),
                summary: String::new(),
                input: input.clone(),
                state: *state,
                output: None,
                took: None,
                background: false,
            })
            .collect()
    }

    #[test]
    fn only_finished_edit_tools_in_the_transcript_count() {
        use serde_json::json;
        let items = items(&[
            (
                "Edit",
                json!({"file_path":"/p/a.rs","old_string":"a\n","new_string":"b\nc\n"}),
                ToolState::Done,
            ),
            (
                "Write",
                json!({"file_path":"rel/w.txt","content":"1\n2\n3\n"}),
                ToolState::Done,
            ),
            (
                "Edit",
                json!({"file_path":"/p/skipped.rs","old_string":"a\n","new_string":"b\n"}),
                ToolState::Denied,
            ),
            ("Bash", json!({"command":"rm x"}), ToolState::Done),
            (
                "File change",
                json!([{"path":"/p/c.txt","kind":{"type":"add"},"diff":"x\ny\n"}]),
                ToolState::Done,
            ),
        ]);
        let summary = estimate_from_items(&items, Path::new("/p"));
        let got: Vec<_> = summary
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.added, f.removed))
            .collect();
        assert_eq!(got, [("a.rs", 2, 1), ("c.txt", 2, 0), ("rel/w.txt", 3, 0)]);
    }

    #[test]
    fn a_git_diff_is_counted_per_file_with_renames_binaries_and_odd_names() {
        let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-one\n+two\n\
diff --git a/b.txt b/b.txt\ndeleted file mode 100644\n--- a/b.txt\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-x\n---- not a header\n-z\n\
diff --git a/bin.dat b/bin.dat\nnew file mode 100644\nBinary files /dev/null and b/bin.dat differ\n\
diff --git a/dir with space/e.txt b/dir with space/e.txt\nnew file mode 100644\n\
diff --git a/r.txt b/r2.txt\nrename from r.txt\nrename to r2.txt\n\
diff --git a/m.sh b/m.sh\nold mode 100644\nnew mode 100755\n";
        let summary = count_git_diff(diff.as_bytes()).unwrap();
        let got: Vec<_> = summary
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.kind, f.added, f.removed, f.binary))
            .collect();
        assert_eq!(
            got,
            [
                ("a.txt", ChangeKind::Modified, 1, 1, false),
                ("b.txt", ChangeKind::Deleted, 0, 3, false),
                ("bin.dat", ChangeKind::Added, 0, 0, true),
                ("dir with space/e.txt", ChangeKind::Added, 0, 0, false),
                ("m.sh", ChangeKind::Modified, 0, 0, false),
                ("r2.txt", ChangeKind::Renamed, 0, 0, false),
            ]
        );
        assert_eq!(summary.files[5].old_path.as_deref(), Some("r.txt"));
    }

    #[test]
    fn a_huge_line_does_not_grow_memory_or_confuse_the_count() {
        let mut diff = String::from(
            "diff --git a/min.js b/min.js\n--- a/min.js\n+++ b/min.js\n@@ -1 +1 @@\n-",
        );
        diff.push_str(&"x".repeat(5_000_000));
        diff.push_str("\n+y\n");
        let summary = count_git_diff(diff.as_bytes()).unwrap();
        assert_eq!((summary.files[0].added, summary.files[0].removed), (1, 1));
    }
}
