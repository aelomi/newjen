use std::io::Read;
use std::path::PathBuf;

const LIMIT: usize = 100_000;

#[derive(Debug, Clone)]
pub struct EditProposal {
    pub path: PathBuf,
    pub change: FileChange,
}

#[derive(Debug, Clone)]
pub enum FileChange {
    Write(String),
    Replace(Vec<Replacement>),
    Patch(String),
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct Replacement {
    pub old: String,
    pub new: String,
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditPreview {
    Patch(String),
    TooLarge,
    Unavailable(String),
}

/// A preview together with the state of the file it was read against. Two reads are equal
/// only if the file is still as it was, so an approval given on a stale read is caught even
/// when the preview itself does not depend on the file (a provider-supplied patch, or an
/// oversized edit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadEdit {
    pub preview: EditPreview,
    file: FileState,
}

/// The file's size and a hash of its bytes, or `None` when it does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState(Option<(u64, u64)>);

impl FileState {
    fn read(path: &std::path::Path) -> FileState {
        use std::hash::Hasher;
        let Ok(mut file) = std::fs::File::open(path) else {
            return FileState(None);
        };
        let mut hasher = std::hash::DefaultHasher::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut length = 0u64;
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    hasher.write(&buffer[..n]);
                    length += n as u64;
                }
                Err(error) => {
                    hasher.write(error.to_string().as_bytes());
                    break;
                }
            }
        }
        FileState(Some((length, hasher.finish())))
    }
}

impl EditProposal {
    /// The preview and the file as it is now; compare two of these to tell whether an
    /// approval still applies to what was reviewed.
    pub fn read(&self) -> ReadEdit {
        ReadEdit {
            preview: self.preview(),
            file: FileState::read(&self.path),
        }
    }

    pub fn preview(&self) -> EditPreview {
        if let FileChange::Patch(patch) = &self.change {
            return if patch.len() > LIMIT {
                EditPreview::TooLarge
            } else {
                EditPreview::Patch(patch.clone())
            };
        }
        if matches!(self.change, FileChange::Unavailable) {
            return EditPreview::Unavailable("The provider supplied no edit preview.".into());
        }
        let mut original = Vec::new();
        match std::fs::File::open(&self.path) {
            Ok(file) => {
                if let Err(error) = file.take((LIMIT + 1) as u64).read_to_end(&mut original) {
                    return EditPreview::Unavailable(format!(
                        "Couldn't read the original file: {error}"
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !matches!(self.change, FileChange::Write(_)) {
                    return EditPreview::Unavailable("The original file is missing.".into());
                }
            }
            Err(error) => {
                return EditPreview::Unavailable(format!(
                    "Couldn't read the original file: {error}"
                ));
            }
        }
        if original.len() > LIMIT {
            return EditPreview::TooLarge;
        }
        let Ok(original) = String::from_utf8(original) else {
            return EditPreview::Unavailable("The original file is not UTF-8 text.".into());
        };
        let modified = match &self.change {
            FileChange::Write(content) => content.clone(),
            FileChange::Replace(edits) => {
                let mut content = original.clone();
                for edit in edits {
                    let count = content.matches(&edit.old).count();
                    if edit.old.is_empty() || count == 0 || (!edit.all && count != 1) {
                        return EditPreview::Unavailable(
                            "The proposed replacement does not match the current file uniquely."
                                .into(),
                        );
                    }
                    content = if edit.all {
                        content.replace(&edit.old, &edit.new)
                    } else {
                        content.replacen(&edit.old, &edit.new, 1)
                    };
                    if content.len() > LIMIT {
                        return EditPreview::TooLarge;
                    }
                }
                content
            }
            FileChange::Patch(_) | FileChange::Unavailable => unreachable!(),
        };
        if original.len() + modified.len() > LIMIT {
            return EditPreview::TooLarge;
        }
        let name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let name = serde_json::to_string(&format!("a/{name}")).expect("string");
        let new_name = name.replacen("a/", "b/", 1);
        let old_lines = original.lines().count();
        let new_lines = modified.lines().count();
        let mut patch = format!(
            "diff --git {name} {new_name}\n--- {name}\n+++ {new_name}\n@@ -{},{} +{},{} @@\n",
            usize::from(old_lines != 0),
            old_lines,
            usize::from(new_lines != 0),
            new_lines
        );
        for (prefix, text) in [('-', &original), ('+', &modified)] {
            for line in text.split_inclusive('\n') {
                patch.push(prefix);
                patch.push_str(line.strip_suffix('\n').unwrap_or(line));
                patch.push('\n');
            }
            if !text.is_empty() && !text.ends_with('\n') {
                patch.push_str("\\ No newline at end of file\n");
            }
        }
        EditPreview::Patch(patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_provider_preview_is_honest() {
        let proposal = EditProposal {
            path: PathBuf::new(),
            change: FileChange::Unavailable,
        };
        assert!(matches!(proposal.preview(), EditPreview::Unavailable(_)));
    }
    #[test]
    fn replacements_use_the_real_file_and_reject_stale_input() {
        let path = std::env::temp_dir().join(format!("newjen-edit-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let proposal = EditProposal {
            path: path.clone(),
            change: FileChange::Replace(vec![Replacement {
                old: "two".into(),
                new: "three".into(),
                all: false,
            }]),
        };
        let EditPreview::Patch(patch) = proposal.preview() else {
            panic!("preview")
        };
        assert!(patch.contains("-two\n"));
        assert!(patch.contains("+three\n"));
        std::fs::write(&path, "changed elsewhere").unwrap();
        assert!(matches!(proposal.preview(), EditPreview::Unavailable(_)));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn a_preview_keeps_carriage_returns_visible_in_the_patch() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&path, "one\r\ntwo\r\n").unwrap();
        let proposal = EditProposal {
            path: path.clone(),
            change: FileChange::Write("one\ntwo\n".into()),
        };
        let EditPreview::Patch(patch) = proposal.preview() else {
            panic!("patch");
        };
        assert!(patch.contains("-one\r\n-two\r\n"));
        assert!(patch.contains("+one\n+two\n"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_large_write_is_never_rendered_as_a_complete_preview() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let proposal = EditProposal {
            path,
            change: FileChange::Write("x".repeat(LIMIT + 1)),
        };
        assert_eq!(proposal.preview(), EditPreview::TooLarge);
    }

    #[test]
    fn a_read_changes_with_the_file_even_when_the_preview_does_not() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&path, "before\n").unwrap();
        for change in [
            FileChange::Patch("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-before\n+after\n".into()),
            FileChange::Write("x".repeat(LIMIT + 1)),
        ] {
            let proposal = EditProposal {
                path: path.clone(),
                change,
            };
            let reviewed = proposal.read();
            assert_eq!(
                proposal.read(),
                reviewed,
                "an unchanged file reads the same"
            );
            std::fs::write(&path, "someone else's work\n").unwrap();
            let now = proposal.read();
            assert_eq!(now.preview, reviewed.preview);
            assert_ne!(now, reviewed, "the changed file must need another review");
            std::fs::write(&path, "before\n").unwrap();
        }
        std::fs::remove_file(path).unwrap();
    }
}
