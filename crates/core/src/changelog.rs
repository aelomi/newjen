//! The release notes in `CHANGELOG.md`, bundled into the app at build time so "What's New"
//! works offline.
//!
//! The file has one `## <version> · <date>` section per release, newest first, and a
//! `## Unreleased` section at the top. A section may open with a paragraph, then holds
//! `### New features`, `### Improvements`, `### Fixes` and `### Misc` groups of `- ` bullets.
//! A bullet may wrap onto following lines and may contain `` `code` ``.

use std::sync::OnceLock;

use semver::Version;

use crate::store::Store;

const BUNDLED_TEXT: &str = include_str!("../../../CHANGELOG.md");

/// The four groups a section can hold, in the order they are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    NewFeatures,
    Improvements,
    Fixes,
    Misc,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::NewFeatures => "New features",
            Group::Improvements => "Improvements",
            Group::Fixes => "Fixes",
            Group::Misc => "Misc",
        }
    }

    fn from_title(title: &str) -> Option<Group> {
        [
            Group::NewFeatures,
            Group::Improvements,
            Group::Fixes,
            Group::Misc,
        ]
        .into_iter()
        .find(|group| group.title() == title)
    }
}

/// The bullets of one group, each as written (inline `code` still in backticks; see
/// [`inline_spans`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteGroup {
    pub group: Group,
    pub items: Vec<String>,
}

/// One section of the changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseNotes {
    /// `None` for `## Unreleased`.
    pub version: Option<Version>,
    /// The heading before the date: `Unreleased` or the version as written.
    pub label: String,
    pub date: Option<String>,
    /// The paragraph under the heading, before any group.
    pub summary: Option<String>,
    /// Only the groups that have bullets, in the order they appear.
    pub groups: Vec<NoteGroup>,
}

/// The notes for the version you now run, and the versions you went past to get here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsNew {
    pub notes: ReleaseNotes,
    /// Released versions after the last one seen and before the current one, newest first.
    /// Empty when the user updated one version at a time.
    pub skipped: Vec<Version>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changelog {
    releases: Vec<ReleaseNotes>,
}

impl Changelog {
    pub fn parse(text: &str) -> Changelog {
        let mut parser = Parser::default();
        for line in text.lines() {
            parser.line(line);
        }
        parser.finish()
    }

    /// Every section, as ordered in the file.
    pub fn releases(&self) -> &[ReleaseNotes] {
        &self.releases
    }

    pub fn notes_for(&self, version: &Version) -> Option<&ReleaseNotes> {
        self.releases
            .iter()
            .find(|notes| notes.version.as_ref() == Some(version))
    }

    /// The highest released version's notes. `Unreleased` never counts.
    pub fn newest_released(&self) -> Option<&ReleaseNotes> {
        self.releases
            .iter()
            .filter(|notes| notes.version.is_some())
            .max_by(|a, b| a.version.cmp(&b.version))
    }

    /// What to show after an update from `last_seen` to `current`. `None` when nothing is new
    /// or the changelog has no section for `current`.
    pub fn whats_new(&self, last_seen: &Version, current: &Version) -> Option<WhatsNew> {
        if last_seen >= current {
            return None;
        }
        let notes = self.notes_for(current)?.clone();
        let mut skipped: Vec<Version> = self
            .releases
            .iter()
            .filter_map(|notes| notes.version.clone())
            .filter(|version| version > last_seen && version < current)
            .collect();
        skipped.sort_by(|a, b| b.cmp(a));
        Some(WhatsNew { notes, skipped })
    }
}

/// The changelog that was compiled into this build.
pub fn bundled() -> &'static Changelog {
    static BUNDLED: OnceLock<Changelog> = OnceLock::new();
    BUNDLED.get_or_init(|| Changelog::parse(BUNDLED_TEXT))
}

/// The bundled notes for `version`.
pub fn notes_for(version: &Version) -> Option<ReleaseNotes> {
    bundled().notes_for(version).cloned()
}

/// The bundled notes of the newest released version.
pub fn newest_released_notes() -> Option<ReleaseNotes> {
    bundled().newest_released().cloned()
}

/// What's New for an update from `last_seen` to `current`, from the bundled changelog.
pub fn whats_new(last_seen: &Version, current: &Version) -> Option<WhatsNew> {
    bundled().whats_new(last_seen, current)
}

/// A run of bullet text, plain or `code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub code: bool,
}

/// Splits bullet text at its backticks. A backtick with no partner stays in the text.
pub fn inline_spans(text: &str) -> Vec<Span> {
    let parts: Vec<&str> = text.split('`').collect();
    let last = parts.len() - 1;
    parts
        .into_iter()
        .enumerate()
        .filter_map(|(index, part)| {
            let code = index % 2 == 1;
            if code && index == last {
                Some(Span {
                    text: format!("`{part}"),
                    code: false,
                })
            } else if part.is_empty() {
                None
            } else {
                Some(Span {
                    text: part.to_owned(),
                    code,
                })
            }
        })
        .collect()
}

#[derive(Default)]
struct Parser {
    releases: Vec<ReleaseNotes>,
    current: Option<ReleaseNotes>,
    group: Option<usize>,
    /// Whether the section has reached its first `###`; text after that is never the summary.
    in_groups: bool,
    /// Set after a bullet or summary line, cleared by a blank line: only then does a plain
    /// line continue the text above it.
    continuing: Continuing,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Continuing {
    #[default]
    No,
    Item,
    Summary,
}

impl Parser {
    fn line(&mut self, line: &str) {
        let trimmed = line.trim();
        if let Some(heading) = line.strip_prefix("## ") {
            self.start_release(heading.trim());
        } else if let Some(heading) = line.strip_prefix("### ") {
            self.start_group(heading.trim());
        } else if trimmed.is_empty() || line.starts_with('#') {
            self.continuing = Continuing::No;
        } else if let Some(item) = line.strip_prefix("- ") {
            self.add_item(item.trim());
        } else {
            self.add_text(trimmed);
        }
    }

    fn start_release(&mut self, heading: &str) {
        self.flush();
        self.continuing = Continuing::No;
        self.group = None;
        self.in_groups = false;
        let (label, date) = match heading.split_once(" · ") {
            Some((label, date)) => (label.trim(), Some(date.trim().to_owned())),
            None => (heading, None),
        };
        let version = label
            .strip_prefix('v')
            .unwrap_or(label)
            .parse::<Version>()
            .ok();
        if version.is_none() && label != "Unreleased" {
            return;
        }
        self.current = Some(ReleaseNotes {
            version,
            label: label.to_owned(),
            date,
            summary: None,
            groups: Vec::new(),
        });
    }

    fn start_group(&mut self, title: &str) {
        self.continuing = Continuing::No;
        self.group = None;
        self.in_groups = true;
        let (Some(release), Some(group)) = (self.current.as_mut(), Group::from_title(title)) else {
            return;
        };
        self.group = Some(
            release
                .groups
                .iter()
                .position(|existing| existing.group == group)
                .unwrap_or_else(|| {
                    release.groups.push(NoteGroup {
                        group,
                        items: Vec::new(),
                    });
                    release.groups.len() - 1
                }),
        );
    }

    fn add_item(&mut self, item: &str) {
        let (Some(release), Some(group)) = (self.current.as_mut(), self.group) else {
            self.continuing = Continuing::No;
            return;
        };
        release.groups[group].items.push(item.to_owned());
        self.continuing = Continuing::Item;
    }

    fn add_text(&mut self, text: &str) {
        let Some(release) = self.current.as_mut() else {
            return;
        };
        match (self.continuing, self.group) {
            (Continuing::Item, Some(group)) => {
                if let Some(item) = release.groups[group].items.last_mut() {
                    item.push(' ');
                    item.push_str(text);
                }
            }
            (_, None) if !self.in_groups => {
                let summary = release.summary.get_or_insert_with(String::new);
                if !summary.is_empty() {
                    summary.push_str(if self.continuing == Continuing::Summary {
                        " "
                    } else {
                        "\n\n"
                    });
                }
                summary.push_str(text);
                self.continuing = Continuing::Summary;
            }
            _ => {}
        }
    }

    fn flush(&mut self) {
        if let Some(mut release) = self.current.take() {
            release.groups.retain(|group| !group.items.is_empty());
            self.releases.push(release);
        }
    }

    fn finish(mut self) -> Changelog {
        self.flush();
        Changelog {
            releases: self.releases,
        }
    }
}

/// The last release made before Newjen recorded what you had seen. Someone who ran it has
/// projects or threads but no recorded version.
pub fn before_whats_new() -> Version {
    Version::parse("0.1.0-alpha.1").expect("a version")
}

/// What to show at launch. With no version recorded and nothing in the data folder from
/// before `launched_at` (Unix milliseconds, taken when the app started) it is a fresh install:
/// it records `current` and returns `None`, so a new user sees no What's New. A data folder
/// that was used before the first updater version counts as having seen [`before_whats_new`].
/// After an update it returns the notes; the caller records the version with [`mark_seen`]
/// once the user has seen them.
pub fn whats_new_on_launch(store: &Store, current: &Version, launched_at: i64) -> Option<WhatsNew> {
    let last_seen = match store.last_seen_version() {
        Ok(None) if store.has_history_before(launched_at).unwrap_or(false) => {
            Ok(Some(before_whats_new()))
        }
        other => other,
    };
    match last_seen {
        Ok(None) => {
            store.set_last_seen_version(current);
            None
        }
        Ok(Some(last_seen)) if last_seen < *current => {
            let news = whats_new(&last_seen, current);
            if news.is_none() {
                store.set_last_seen_version(current);
            }
            news
        }
        Ok(Some(last_seen)) => {
            if last_seen > *current {
                store.set_last_seen_version(current);
            }
            None
        }
        Err(_) => None,
    }
}

/// Records that the notes for `current` were shown.
pub fn mark_seen(store: &Store, current: &Version) {
    store.set_last_seen_version(current);
}
