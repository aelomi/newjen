//! Small values that belong to the whole app, not to a project or thread.

use anyhow::Result;
use rusqlite::OptionalExtension;
use semver::Version;

use super::{Store, Write};

const LAST_SEEN_VERSION: &str = "last_seen_version";
const FAILED_UPDATE_VERSION: &str = "failed_update_version";

impl Store {
    /// The newest version whose release notes the user has been shown, or `None` before the
    /// first launch has recorded one.
    pub fn last_seen_version(&self) -> Result<Option<Version>> {
        self.app_state_version(LAST_SEEN_VERSION)
    }

    /// The version whose install failed, if one did. The quiet hourly check leaves it and
    /// everything below it alone, so a release that cannot be installed is not downloaded
    /// again every launch.
    pub fn failed_update_version(&self) -> Result<Option<Version>> {
        self.app_state_version(FAILED_UPDATE_VERSION)
    }

    pub fn set_failed_update_version(&self, version: &Version) {
        self.queue(Write::AppState {
            key: FAILED_UPDATE_VERSION,
            value: version.to_string(),
        });
    }

    /// Whether this data folder was used before `launched_at` (Unix milliseconds): it holds a
    /// project or a thread made earlier. Only the first launch of a new install finds it
    /// empty, and a project that launch opens itself does not count.
    pub fn has_history_before(&self, launched_at: i64) -> Result<bool> {
        Ok(self.reader().query_row(
            "SELECT EXISTS (SELECT 1 FROM projects WHERE created_at < ?1)
                 OR EXISTS (SELECT 1 FROM threads WHERE created_at < ?1)",
            [launched_at],
            |row| row.get(0),
        )?)
    }

    fn app_state_version(&self, key: &str) -> Result<Option<Version>> {
        let text: Option<String> = self
            .reader()
            .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(text.and_then(|text| text.parse().ok()))
    }

    pub fn set_last_seen_version(&self, version: &Version) {
        self.queue(Write::AppState {
            key: LAST_SEEN_VERSION,
            value: version.to_string(),
        });
    }
}
