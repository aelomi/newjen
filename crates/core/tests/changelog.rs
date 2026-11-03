//! Parsing `CHANGELOG.md`, what's new after an update, and the last-seen version.

use newjen_core::changelog::{
    Changelog, Group, Span, bundled, inline_spans, mark_seen, newest_released_notes, notes_for,
    whats_new_on_launch,
};
use newjen_core::store::Store;
use semver::Version;

const LATER: i64 = i64::MAX;

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

const SAMPLE: &str = "# Changelog

Intro that belongs to no release.

## Unreleased

### New features

- A thing with `inline code` in it.
- A bullet that wraps
  onto a second line
  and a third.

### Fixes

- Fixed it.

## 0.3.0 · 3 November 2026

### Improvements

- Faster.

### Misc

- Tidied.

## 0.2.0 · 2 November 2026

The second release, as one paragraph
that wraps.

### Fixes

- Second fix.
- Another second fix.

### Surprises

- Not a known group.
  Its wrapped line must not become a summary.

## 0.1.0-beta.1 · 1 November 2026

### New features

- Beta thing.

## 0.1.0-alpha.1 · 30 October 2026

The first pre-release: a paragraph and no groups.

## Not a version

- ignored
";

fn sample() -> Changelog {
    Changelog::parse(SAMPLE)
}

#[test]
fn sections_groups_and_bullets_are_parsed() {
    let changelog = sample();
    let labels: Vec<&str> = changelog
        .releases()
        .iter()
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "Unreleased",
            "0.3.0",
            "0.2.0",
            "0.1.0-beta.1",
            "0.1.0-alpha.1"
        ]
    );

    let unreleased = &changelog.releases()[0];
    assert_eq!(unreleased.version, None);
    assert_eq!(unreleased.date, None);
    assert_eq!(unreleased.groups.len(), 2);
    assert_eq!(unreleased.groups[0].group, Group::NewFeatures);
    assert_eq!(
        unreleased.groups[0].items,
        [
            "A thing with `inline code` in it.",
            "A bullet that wraps onto a second line and a third."
        ]
    );

    let second = changelog.notes_for(&v("0.2.0")).unwrap();
    assert_eq!(second.date.as_deref(), Some("2 November 2026"));
    assert_eq!(
        second.summary.as_deref(),
        Some("The second release, as one paragraph that wraps.")
    );
    assert_eq!(second.groups.len(), 1, "an unknown group is dropped");
    assert_eq!(second.groups[0].items.len(), 2);

    let first = changelog.notes_for(&v("0.1.0-alpha.1")).unwrap();
    assert_eq!(
        first.summary.as_deref(),
        Some("The first pre-release: a paragraph and no groups.")
    );
    assert!(first.groups.is_empty());
}

#[test]
fn a_version_matches_only_its_own_section() {
    let changelog = sample();
    assert!(changelog.notes_for(&v("0.1.0")).is_none());
    assert!(changelog.notes_for(&v("0.1.0-beta.1")).is_some());
    assert!(changelog.notes_for(&v("0.1.0-alpha.1")).is_some());
}

#[test]
fn the_newest_released_version_skips_unreleased() {
    assert_eq!(sample().newest_released().unwrap().label, "0.3.0");
}

#[test]
fn whats_new_lists_the_versions_the_user_skipped() {
    let changelog = sample();
    let news = changelog
        .whats_new(&v("0.1.0-alpha.1"), &v("0.3.0"))
        .unwrap();
    assert_eq!(news.notes.label, "0.3.0");
    assert_eq!(news.skipped, [v("0.2.0"), v("0.1.0-beta.1")]);

    let one_step = changelog.whats_new(&v("0.2.0"), &v("0.3.0")).unwrap();
    assert!(one_step.skipped.is_empty());

    assert!(changelog.whats_new(&v("0.3.0"), &v("0.3.0")).is_none());
    assert!(changelog.whats_new(&v("0.3.0"), &v("0.2.0")).is_none());
    assert!(
        changelog
            .whats_new(&v("0.1.0-alpha.1"), &v("0.2.5"))
            .is_none(),
        "no section for it"
    );
}

#[test]
fn inline_code_splits_into_spans() {
    let plain = |text: &str| Span {
        text: text.into(),
        code: false,
    };
    let code = |text: &str| Span {
        text: text.into(),
        code: true,
    };
    assert_eq!(
        inline_spans("Run `cargo test` now"),
        [plain("Run "), code("cargo test"), plain(" now")]
    );
    assert_eq!(inline_spans("no code"), [plain("no code")]);
    assert_eq!(inline_spans("a ` stray"), [plain("a "), plain("` stray")]);
    assert_eq!(inline_spans("`one``two`"), [code("one"), code("two")]);
}

#[test]
fn the_bundled_changelog_parses_completely() {
    let text = include_str!("../../../CHANGELOG.md");
    let changelog = bundled();
    let parsed_items: usize = changelog
        .releases()
        .iter()
        .flat_map(|notes| &notes.groups)
        .map(|group| group.items.len())
        .sum();
    let bullets = text.lines().filter(|line| line.starts_with("- ")).count();
    assert_eq!(parsed_items, bullets, "a bullet sits outside a known group");
    for heading in text.lines().filter_map(|line| line.strip_prefix("### ")) {
        assert!(
            [
                Group::NewFeatures,
                Group::Improvements,
                Group::Fixes,
                Group::Misc
            ]
            .iter()
            .any(|group| group.title() == heading),
            "unknown group heading: {heading}"
        );
    }
    for heading in text.lines().filter_map(|line| line.strip_prefix("## ")) {
        assert!(
            heading == "Unreleased"
                || heading
                    .split_once(" · ")
                    .is_some_and(|(version, _)| version.parse::<Version>().is_ok()),
            "unreadable section heading: {heading}"
        );
    }
    let unreleased = changelog
        .releases()
        .iter()
        .filter(|notes| notes.version.is_none())
        .count();
    assert!(unreleased <= 1, "more than one Unreleased section");
    for notes in changelog.releases().iter().filter(|n| n.version.is_some()) {
        assert!(notes.date.is_some(), "{} has no date", notes.label);
        assert!(
            notes.summary.is_some() || notes.groups.iter().any(|group| !group.items.is_empty()),
            "{} has no notes",
            notes.label
        );
    }
    let newest = newest_released_notes().expect("a released version");
    assert!(newest.version.as_ref().unwrap() >= &v("0.1.0-alpha.1"));
    assert!(notes_for(&v("0.1.0-alpha.1")).is_some());
}

#[test]
fn a_fresh_install_records_its_version_silently_and_an_update_shows_the_notes() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let flush = || assert!(store.flush(std::time::Duration::from_secs(5)));
    let current = v("0.1.0-alpha.1");

    assert_eq!(store.last_seen_version().unwrap(), None);
    assert!(whats_new_on_launch(&store, &current, LATER).is_none());
    flush();
    assert_eq!(store.last_seen_version().unwrap(), Some(current.clone()));
    assert!(
        whats_new_on_launch(&store, &current, LATER).is_none(),
        "same version, nothing new"
    );

    store.set_last_seen_version(&v("0.0.9"));
    flush();
    let news = whats_new_on_launch(&store, &current, LATER).expect("the notes for the new version");
    assert_eq!(news.notes.version, Some(current.clone()));
    flush();
    assert_eq!(
        store.last_seen_version().unwrap(),
        Some(v("0.0.9")),
        "not recorded until seen"
    );

    mark_seen(&store, &current);
    flush();
    assert_eq!(store.last_seen_version().unwrap(), Some(current));
}

#[test]
fn a_data_folder_from_before_the_updater_counts_as_having_seen_alpha_1() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    store
        .open_project(std::path::Path::new("/tmp/an-older-project"))
        .unwrap();
    assert_eq!(store.last_seen_version().unwrap(), None);
    assert!(store.has_history_before(LATER).unwrap());

    let news = whats_new_on_launch(&store, &v("0.1.0-alpha.2"), LATER)
        .expect("an existing user gets the notes of their first updater version");
    assert_eq!(news.notes.version, Some(v("0.1.0-alpha.2")));
    assert!(news.skipped.is_empty());
    assert!(store.flush(std::time::Duration::from_secs(5)));
    assert_eq!(
        store.last_seen_version().unwrap(),
        None,
        "not recorded until seen"
    );
}

#[test]
fn an_empty_data_folder_has_no_history() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    assert!(!store.has_history_before(LATER).unwrap());
}

#[test]
fn the_project_a_first_launch_opens_does_not_make_it_look_used() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let launched_at = newjen_core::store::now_ms() - 5;
    store
        .open_project(std::path::Path::new("/tmp/the-first-project"))
        .unwrap();
    assert!(!store.has_history_before(launched_at).unwrap());
    assert!(whats_new_on_launch(&store, &v("0.1.0-alpha.2"), launched_at).is_none());
    assert!(store.flush(std::time::Duration::from_secs(5)));
    assert_eq!(store.last_seen_version().unwrap(), Some(v("0.1.0-alpha.2")));
}
