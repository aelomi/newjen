//! Usage rows and the questions the Usage view asks of them.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, Row, params};

use super::Store;
use crate::agent::TokenCounts;
use crate::providers::{Billing, Provider};
use crate::thread_usage::TurnUsageRow;
use crate::usage::{ProviderUsage, SavedUsage};

const SECONDS_PER_DAY: i64 = 86_400;

/// One thread's use of a provider since a moment, over all the models it ran there.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadWindowUsage {
    pub thread_id: String,
    pub project_id: i64,
    pub title: String,
    /// An archived thread is hidden from the thread list but its tokens still count.
    pub archived: bool,
    /// Models the thread used, the one with the most tokens first.
    pub models: Vec<String>,
    pub tokens: TokenCounts,
    /// `None` when no turn reported a cost.
    pub cost_usd: Option<f64>,
    /// When the last of those turns ended, Unix milliseconds.
    pub last_active_ms: i64,
}

/// One provider's tokens over one local calendar day.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyTokens {
    /// Local midnight that starts the day, as Unix seconds.
    pub day_start_unix_seconds: i64,
    pub provider: Provider,
    pub tokens: TokenCounts,
}

impl Store {
    /// The usage rows a thread has saved, in no particular order.
    pub fn turn_usage(&self, thread_id: &str) -> Result<Vec<TurnUsageRow>> {
        let connection = self.reader();
        let mut statement = connection.prepare(
            "SELECT turn, ended_at, provider, billing, model, input, cached_input, cache_write_input,
                    output, reasoning_output, cost_usd, context_used, context_window
             FROM turn_usage WHERE thread_id = ?1 ORDER BY turn, model",
        )?;
        let rows = statement.query_map([thread_id], turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The plan reports saved for each provider, as the last run left them.
    pub fn provider_usages(&self) -> Result<Vec<SavedUsage>> {
        let connection = self.reader();
        let mut statement = connection.prepare("SELECT report, account FROM provider_usage")?;
        let reports = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        Ok(reports
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(report, account)| {
                Some(SavedUsage {
                    usage: serde_json::from_str(&report).ok()?,
                    account,
                })
            })
            .collect())
    }

    /// Keeps the provider's latest plan report, or forgets it when there is none.
    pub(crate) fn save_provider_usage(
        &self,
        provider: Provider,
        usage: Option<&ProviderUsage>,
        account: Option<&str>,
    ) {
        self.queue(super::Write::ProviderUsage {
            provider: provider.id().into(),
            report: usage.map(|usage| serde_json::to_string(usage).expect("usage serialization")),
            account: account.map(str::to_owned),
            updated_at: super::now_ms(),
        });
    }

    /// Saves usage rows for a thread, replacing any saved for the same turn and model.
    pub(crate) fn save_turn_usage(&self, thread_id: &str, rows: Vec<TurnUsageRow>) {
        self.queue(super::Write::TurnUsage {
            thread_id: thread_id.into(),
            rows,
        });
    }

    /// Every thread that used `provider` at or after `since_ms`, with what it used then. The
    /// threads that used the most tokens come first.
    pub fn threads_with_usage_since(
        &self,
        provider: Provider,
        since_ms: i64,
    ) -> Result<Vec<ThreadWindowUsage>> {
        let connection = self.reader();
        let mut statement = connection.prepare(
            "SELECT threads.id, threads.project_id, threads.title, threads.archived_at IS NOT NULL,
                    turn_usage.model, SUM(input), SUM(cached_input), SUM(cache_write_input),
                    SUM(output), SUM(reasoning_output), SUM(cost_usd), MAX(ended_at)
             FROM turn_usage JOIN threads ON threads.id = turn_usage.thread_id
             WHERE turn_usage.provider = ?1 AND ended_at >= ?2
             GROUP BY threads.id, turn_usage.model",
        )?;
        let per_model = statement
            .query_map(params![provider.id(), since_ms], |row| {
                Ok((
                    ThreadWindowUsage {
                        thread_id: row.get(0)?,
                        project_id: row.get(1)?,
                        title: row.get(2)?,
                        archived: row.get(3)?,
                        models: Vec::new(),
                        tokens: token_counts(row, 5)?,
                        cost_usd: row.get(10)?,
                        last_active_ms: row.get(11)?,
                    },
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut threads: Vec<ThreadWindowUsage> = Vec::new();
        let mut model_tokens: HashMap<String, Vec<(String, u64)>> = HashMap::new();
        for (usage, model) in per_model {
            model_tokens
                .entry(usage.thread_id.clone())
                .or_default()
                .push((model, usage.tokens.total()));
            match threads.iter_mut().find(|t| t.thread_id == usage.thread_id) {
                Some(thread) => {
                    thread.tokens = thread.tokens.saturating_add(usage.tokens);
                    thread.cost_usd = match (thread.cost_usd, usage.cost_usd) {
                        (None, None) => None,
                        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
                    };
                    thread.last_active_ms = thread.last_active_ms.max(usage.last_active_ms);
                }
                None => threads.push(usage),
            }
        }
        for thread in &mut threads {
            let mut models = model_tokens.remove(&thread.thread_id).unwrap_or_default();
            models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            thread.models = models.into_iter().map(|(model, _)| model).collect();
        }
        threads.sort_by(|a, b| {
            b.tokens
                .total()
                .cmp(&a.tokens.total())
                .then_with(|| b.last_active_ms.cmp(&a.last_active_ms))
                .then_with(|| a.thread_id.cmp(&b.thread_id))
        });
        Ok(threads)
    }

    /// Tokens per provider for each of the last `days` local days, oldest first, today last.
    /// Every day has an entry for every provider, empty when nothing ran.
    /// `utc_offset_at` gives the local time zone's offset at a Unix instant (see
    /// [`local_utc_offset_seconds`]), so a day that a clock change shortens or lengthens still
    /// starts at its own local midnight.
    pub fn daily_tokens(
        &self,
        now_ms: i64,
        days: u32,
        utc_offset_at: impl Fn(i64) -> i32,
    ) -> Result<Vec<DailyTokens>> {
        let starts = local_day_starts(now_ms.div_euclid(1000), days, utc_offset_at);
        let (first, end) = (starts[0], starts[starts.len() - 1]);
        let connection = self.reader();
        let mut statement = connection.prepare(
            "SELECT provider, ended_at, input, cached_input, cache_write_input, output,
                    reasoning_output
             FROM turn_usage WHERE ended_at >= ?1 AND ended_at < ?2",
        )?;
        let rows = statement.query_map(params![first * 1000, end * 1000], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                token_counts(row, 2)?,
            ))
        })?;
        let mut found: HashMap<(usize, Provider), TokenCounts> = HashMap::new();
        for row in rows {
            let (provider, ended_at, tokens) = row?;
            let Some(provider) = Provider::from_id(&provider) else {
                continue;
            };
            let day = starts.partition_point(|start| start * 1000 <= ended_at) - 1;
            let total = found.entry((day, provider)).or_default();
            *total = total.saturating_add(tokens);
        }
        Ok(starts[..starts.len() - 1]
            .iter()
            .enumerate()
            .flat_map(|(day, start)| {
                [Provider::Claude, Provider::Codex].map(|provider| DailyTokens {
                    day_start_unix_seconds: *start,
                    provider,
                    tokens: found.get(&(day, provider)).copied().unwrap_or_default(),
                })
            })
            .collect())
    }

    /// What turns billed to an API key cost since `since_ms`, for a provider whose account is
    /// billed to an API key now. A turn that ran before the billing was known counts too;
    /// subscription turns never do, as their cost is only what the tokens would list for.
    pub fn charged_since(&self, provider: Provider, since_ms: i64) -> Result<f64> {
        Ok(self.reader().query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM turn_usage
             WHERE provider = ?1 AND billing IN ('api_key', 'unknown') AND ended_at >= ?2",
            params![provider.id(), since_ms],
            |row| row.get(0),
        )?)
    }
}

/// The instants, Unix seconds, at which the last `days` local days began and then the one
/// after today does: `days + 1` entries, oldest first.
fn local_day_starts(now: i64, days: u32, utc_offset_at: impl Fn(i64) -> i32) -> Vec<i64> {
    let today = (now + i64::from(utc_offset_at(now))).div_euclid(SECONDS_PER_DAY);
    let first = today - i64::from(days.max(1)) + 1;
    (first..=today + 1)
        .map(|day| {
            let local = day * SECONDS_PER_DAY;
            let mut start = local - i64::from(utc_offset_at(now));
            for _ in 0..3 {
                start = local - i64::from(utc_offset_at(start));
            }
            start
        })
        .collect()
}

/// The local time zone's offset from UTC at `unix_seconds`, for [`Store::daily_tokens`].
pub fn local_utc_offset_seconds(unix_seconds: i64) -> i32 {
    let seconds: libc::time_t = unix_seconds;
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() {
        return 0;
    }
    i32::try_from(unsafe { local.assume_init() }.tm_gmtoff).unwrap_or(0)
}

pub(super) fn write_turn_rows(
    connection: &Connection,
    thread_id: &str,
    rows: &[TurnUsageRow],
) -> rusqlite::Result<()> {
    let mut insert = connection.prepare(
        "INSERT INTO turn_usage (thread_id, turn, model, ended_at, provider, billing, input,
                                 cached_input, cache_write_input, output, reasoning_output,
                                 cost_usd, context_used, context_window)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (thread_id, turn, model) DO UPDATE SET
             ended_at = excluded.ended_at,
             provider = excluded.provider,
             billing = excluded.billing,
             input = excluded.input,
             cached_input = excluded.cached_input,
             cache_write_input = excluded.cache_write_input,
             output = excluded.output,
             reasoning_output = excluded.reasoning_output,
             cost_usd = excluded.cost_usd,
             context_used = excluded.context_used,
             context_window = excluded.context_window",
    )?;
    for row in rows {
        insert.execute(params![
            thread_id,
            row.turn,
            row.model,
            row.ended_at,
            row.provider.id(),
            billing_text(row.billing),
            to_sql(row.tokens.input),
            to_sql(row.tokens.cached_input),
            to_sql(row.tokens.cache_write_input),
            to_sql(row.tokens.output),
            to_sql(row.tokens.reasoning_output),
            row.cost_usd,
            row.context_used.map(to_sql),
            row.context_window.map(to_sql),
        ])?;
    }
    Ok(())
}

fn turn_row(row: &Row<'_>) -> rusqlite::Result<TurnUsageRow> {
    let invalid = |column: usize| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            "unknown value".into(),
        )
    };
    let provider = row.get::<_, String>(2)?;
    let billing = row.get::<_, String>(3)?;
    Ok(TurnUsageRow {
        turn: row.get(0)?,
        ended_at: row.get(1)?,
        provider: Provider::from_id(&provider).ok_or_else(|| invalid(2))?,
        billing: billing_from_text(&billing).ok_or_else(|| invalid(3))?,
        model: row.get(4)?,
        tokens: token_counts(row, 5)?,
        cost_usd: row.get(10)?,
        context_used: row.get::<_, Option<i64>>(11)?.map(from_sql),
        context_window: row.get::<_, Option<i64>>(12)?.map(from_sql),
    })
}

fn token_counts(row: &Row<'_>, first: usize) -> rusqlite::Result<TokenCounts> {
    Ok(TokenCounts {
        input: from_sql(row.get(first)?),
        cached_input: from_sql(row.get(first + 1)?),
        cache_write_input: from_sql(row.get(first + 2)?),
        output: from_sql(row.get(first + 3)?),
        reasoning_output: from_sql(row.get(first + 4)?),
    })
}

fn billing_text(billing: Billing) -> &'static str {
    match billing {
        Billing::Subscription => "subscription",
        Billing::ApiKey => "api_key",
        Billing::Unknown => "unknown",
    }
}

fn billing_from_text(text: &str) -> Option<Billing> {
    match text {
        "subscription" => Some(Billing::Subscription),
        "api_key" => Some(Billing::ApiKey),
        "unknown" => Some(Billing::Unknown),
        _ => None,
    }
}

/// SQLite integers are signed; a count past `i64::MAX` is not a real count.
fn to_sql(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn from_sql(count: i64) -> u64 {
    u64::try_from(count).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::*;
    use crate::store::{DB_FILE, MIGRATIONS, SCHEMA_VERSION, ThreadRow, Unsaved, migrate};

    const HOUR_MS: i64 = 3_600_000;

    fn thread_row(id: &str, project_id: i64) -> ThreadRow {
        ThreadRow {
            id: id.to_string(),
            project_id,
            workdir: PathBuf::from("/tmp/project"),
            title: format!("Thread {id}"),
            provider: "claude".to_string(),
            conversation: crate::agent::ConversationState::Reserved(format!("session-{id}")),
            model: crate::providers::ModelSelection::CliDefault,
            effort: crate::agent::EffortSelection::CliDefault,
            speed: crate::agent::Speed::Normal,
            status: crate::transcript::Status::Done,
            unread: false,
            created_at: 1,
            updated_at: 1,
            archived_at: None,
            copy: None,
            review_base: None,
            landing_resolved: false,
            snapshot_needed: false,
        }
    }

    fn tokens(input: u64, cached: u64, output: u64) -> TokenCounts {
        TokenCounts {
            input,
            cached_input: cached,
            cache_write_input: 0,
            output,
            reasoning_output: 0,
        }
    }

    fn usage_row(
        turn: u32,
        ended_at: i64,
        provider: Provider,
        model: &str,
        tokens: TokenCounts,
        cost: Option<f64>,
    ) -> TurnUsageRow {
        TurnUsageRow {
            turn,
            ended_at,
            provider,
            billing: Billing::Subscription,
            model: model.into(),
            tokens,
            cost_usd: cost,
            context_used: None,
            context_window: None,
        }
    }

    fn open() -> (tempfile::TempDir, Store, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).unwrap();
        let project = store.open_project(Path::new("/tmp/project")).unwrap();
        (dir, store, project.id)
    }

    fn save(store: &Store, thread: &str, rows: Vec<TurnUsageRow>) {
        store.save_turn_usage(thread, rows);
        assert!(store.flush(Duration::from_secs(5)));
    }

    #[test]
    fn a_version_6_database_gains_the_usage_tables_and_keeps_its_threads() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut connection = Connection::open(dir.path().join(DB_FILE)).unwrap();
            migrate(&mut connection, &MIGRATIONS[..6]).unwrap();
            connection
                .execute_batch(
                    "INSERT INTO projects (id, path, created_at, last_opened_at)
                         VALUES (1, '/tmp/project', 1, 1);
                     INSERT INTO threads (id, project_id, workdir, title, session_id, status,
                                          created_at, updated_at)
                         VALUES ('t1', 1, '/tmp/project', 'Old', 's1', 'done', 1, 1);",
                )
                .unwrap();
        }
        let store = Store::open_in(dir.path()).unwrap();
        let connection = Connection::open(store.path()).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!((SCHEMA_VERSION, version), (8, 8));
        assert_eq!(store.thread("t1").unwrap().unwrap().title, "Old");
        assert!(store.turn_usage("t1").unwrap().is_empty());
        save(
            &store,
            "t1",
            vec![usage_row(
                0,
                5,
                Provider::Claude,
                "m",
                tokens(1, 2, 3),
                None,
            )],
        );
        assert_eq!(store.turn_usage("t1").unwrap().len(), 1);
    }

    #[test]
    fn rows_round_trip_and_writing_a_turn_again_replaces_it() {
        let (_dir, store, project) = open();
        store.save_thread(thread_row("t1", project));
        let mut main = usage_row(
            0,
            1000,
            Provider::Claude,
            "main",
            tokens(1, 2, 3),
            Some(0.5),
        );
        main.context_used = Some(77);
        main.context_window = Some(1000);
        main.billing = Billing::ApiKey;
        let sub = usage_row(0, 1000, Provider::Claude, "sub", tokens(4, 5, 6), None);
        save(&store, "t1", vec![main.clone(), sub.clone()]);
        assert_eq!(store.turn_usage("t1").unwrap(), [main.clone(), sub.clone()]);

        let mut redone = main.clone();
        redone.tokens = tokens(10, 20, 30);
        save(&store, "t1", vec![redone.clone(), sub.clone()]);
        assert_eq!(store.turn_usage("t1").unwrap(), [redone, sub]);
    }

    #[test]
    fn a_failed_usage_write_is_remembered_from_its_first_turn_and_changes_nothing() {
        let (_dir, store, _) = open();
        let row = |turn| usage_row(turn, 1, Provider::Claude, "m", tokens(1, 1, 1), None);
        save(&store, "no-such-thread", vec![row(4), row(3), row(5)]);
        assert_eq!(
            store.take_unsaved("no-such-thread"),
            Some(Unsaved {
                usage_from: Some(3),
                ..Unsaved::default()
            })
        );
        assert!(store.write_errors().try_recv().is_ok());
        assert!(store.turn_usage("no-such-thread").unwrap().is_empty());
    }

    #[test]
    fn plan_reports_are_kept_per_provider_replaced_and_forgotten() {
        let (_dir, store, _) = open();
        let mut claude = ProviderUsage::empty(Provider::Claude);
        claude.plan = Some("max".into());
        claude.billing = Billing::Subscription;
        let codex = ProviderUsage::empty(Provider::Codex);
        store.save_provider_usage(Provider::Claude, Some(&claude), Some("a@example.com"));
        store.save_provider_usage(Provider::Codex, Some(&codex), None);
        assert!(store.flush(Duration::from_secs(5)));
        let mut saved = store.provider_usages().unwrap();
        saved.sort_by_key(|saved| saved.usage.provider.id());
        assert_eq!(
            saved,
            [
                SavedUsage {
                    usage: claude.clone(),
                    account: Some("a@example.com".into())
                },
                SavedUsage {
                    usage: codex,
                    account: None
                },
            ]
        );

        claude.plan = Some("pro".into());
        store.save_provider_usage(Provider::Claude, Some(&claude), Some("b@example.com"));
        store.save_provider_usage(Provider::Codex, None, None);
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(
            store.provider_usages().unwrap(),
            [SavedUsage {
                usage: claude,
                account: Some("b@example.com".into())
            }]
        );
    }

    #[test]
    fn usage_goes_with_its_thread_when_the_thread_is_deleted() {
        let (_dir, store, project) = open();
        store.save_thread(thread_row("t1", project));
        save(
            &store,
            "t1",
            vec![usage_row(
                0,
                1,
                Provider::Claude,
                "m",
                tokens(1, 1, 1),
                None,
            )],
        );
        let connection = Connection::open(store.path()).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection
            .execute("DELETE FROM threads WHERE id = 't1'", [])
            .unwrap();
        assert!(store.turn_usage("t1").unwrap().is_empty());
    }

    #[test]
    fn threads_with_usage_in_a_window_are_grouped_per_thread_and_the_biggest_come_first() {
        let (_dir, store, project) = open();
        for id in ["small", "big", "old", "other", "archived"] {
            store.save_thread(thread_row(id, project));
        }
        store.archive("archived", 9);
        let claude = Provider::Claude;
        save(
            &store,
            "small",
            vec![usage_row(
                0,
                5_000,
                claude,
                "a",
                tokens(1, 1, 1),
                Some(0.25),
            )],
        );
        save(
            &store,
            "big",
            vec![
                usage_row(0, 4_000, claude, "a", tokens(10, 100, 10), Some(1.0)),
                usage_row(0, 4_000, claude, "b", tokens(1, 500, 1), None),
                usage_row(1, 6_000, claude, "a", tokens(10, 100, 10), Some(1.0)),
            ],
        );
        save(
            &store,
            "old",
            vec![usage_row(0, 999, claude, "a", tokens(9, 9, 9), Some(9.0))],
        );
        save(
            &store,
            "other",
            vec![usage_row(
                0,
                5_000,
                Provider::Codex,
                "gpt",
                tokens(5, 5, 5),
                None,
            )],
        );
        save(
            &store,
            "archived",
            vec![usage_row(0, 7_000, claude, "c", tokens(2, 2, 2), None)],
        );

        let found = store.threads_with_usage_since(claude, 1_000).unwrap();
        let ids: Vec<_> = found.iter().map(|t| t.thread_id.as_str()).collect();
        assert_eq!(ids, ["big", "archived", "small"]);
        let big = &found[0];
        assert_eq!(big.title, "Thread big");
        assert_eq!(big.project_id, project);
        assert_eq!(big.models, ["b", "a"]);
        assert_eq!(big.tokens, tokens(21, 700, 21));
        assert_eq!(big.cost_usd, Some(2.0));
        assert_eq!(big.last_active_ms, 6_000);
        assert!(!big.archived);
        assert!(found[1].archived);
        assert_eq!(found[1].cost_usd, None);
        assert_eq!(found[2].cost_usd, Some(0.25));

        let codex = store.threads_with_usage_since(Provider::Codex, 0).unwrap();
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].models, ["gpt"]);
    }

    #[test]
    fn daily_tokens_cover_every_local_day_for_both_providers_oldest_first() {
        let (_dir, store, project) = open();
        store.save_thread(thread_row("t1", project));
        let offset = 5 * 3600;
        let today = 20_000i64;
        let midnight_ms = |day: i64| (day * 86_400 - i64::from(offset)) * 1000;
        let now = midnight_ms(today) + 15 * HOUR_MS;
        save(
            &store,
            "t1",
            vec![
                usage_row(
                    0,
                    midnight_ms(today) - 1,
                    Provider::Claude,
                    "a",
                    tokens(1, 0, 1),
                    None,
                ),
                usage_row(
                    1,
                    midnight_ms(today),
                    Provider::Claude,
                    "a",
                    tokens(10, 0, 10),
                    None,
                ),
                usage_row(
                    2,
                    midnight_ms(today) + 2 * HOUR_MS,
                    Provider::Claude,
                    "b",
                    tokens(100, 0, 100),
                    None,
                ),
                usage_row(
                    3,
                    midnight_ms(today) + 2 * HOUR_MS,
                    Provider::Codex,
                    "g",
                    tokens(7, 0, 7),
                    None,
                ),
                usage_row(
                    4,
                    midnight_ms(today - 6) + 1,
                    Provider::Codex,
                    "g",
                    tokens(3, 0, 3),
                    None,
                ),
                usage_row(
                    5,
                    midnight_ms(today - 7) - 1,
                    Provider::Codex,
                    "g",
                    tokens(900, 0, 900),
                    None,
                ),
                usage_row(
                    6,
                    midnight_ms(today + 1),
                    Provider::Codex,
                    "g",
                    tokens(900, 0, 900),
                    None,
                ),
            ],
        );
        let days = store.daily_tokens(now, 7, |_| offset).unwrap();
        assert_eq!(days.len(), 14);
        let starts: Vec<i64> = days
            .iter()
            .step_by(2)
            .map(|day| day.day_start_unix_seconds)
            .collect();
        let expected: Vec<i64> = (today - 6..=today)
            .map(|day| day * 86_400 - i64::from(offset))
            .collect();
        assert_eq!(starts, expected);
        assert!(days.iter().enumerate().all(|(i, day)| day.provider
            == if i % 2 == 0 {
                Provider::Claude
            } else {
                Provider::Codex
            }));
        let on = |day: i64, provider: Provider| {
            days.iter()
                .find(|d| {
                    d.day_start_unix_seconds == day * 86_400 - i64::from(offset)
                        && d.provider == provider
                })
                .unwrap()
                .tokens
        };
        assert_eq!(on(today, Provider::Claude), tokens(110, 0, 110));
        assert_eq!(on(today, Provider::Codex), tokens(7, 0, 7));
        assert_eq!(on(today - 1, Provider::Claude), tokens(1, 0, 1));
        assert_eq!(on(today - 6, Provider::Codex), tokens(3, 0, 3));
        assert_eq!(on(today - 3, Provider::Claude), TokenCounts::default());
    }

    #[test]
    fn turns_billed_to_an_api_key_or_to_nothing_known_yet_count_as_charged() {
        let (_dir, store, project) = open();
        store.save_thread(thread_row("t1", project));
        let mut api = usage_row(0, 5_000, Provider::Claude, "m", tokens(1, 1, 1), Some(0.5));
        api.billing = Billing::ApiKey;
        let mut api_late = usage_row(1, 6_000, Provider::Claude, "m", tokens(1, 1, 1), Some(0.25));
        api_late.billing = Billing::ApiKey;
        let mut api_early = usage_row(2, 100, Provider::Claude, "m", tokens(1, 1, 1), Some(8.0));
        api_early.billing = Billing::ApiKey;
        let subscription = usage_row(3, 5_000, Provider::Claude, "m", tokens(1, 1, 1), Some(4.0));
        let mut unknown = usage_row(4, 5_000, Provider::Claude, "m", tokens(1, 1, 1), Some(2.0));
        unknown.billing = Billing::Unknown;
        let mut codex = usage_row(5, 5_000, Provider::Codex, "g", tokens(1, 1, 1), Some(16.0));
        codex.billing = Billing::ApiKey;
        save(
            &store,
            "t1",
            vec![api, api_late, api_early, subscription, unknown, codex],
        );
        assert_eq!(store.charged_since(Provider::Claude, 1_000).unwrap(), 2.75);
        assert_eq!(store.charged_since(Provider::Claude, 7_000).unwrap(), 0.0);
        assert_eq!(store.charged_since(Provider::Codex, 0).unwrap(), 16.0);
    }

    #[test]
    fn a_clock_change_gives_each_day_its_own_midnight() {
        let (_dir, store, project) = open();
        store.save_thread(thread_row("t1", project));
        let today = 20_000i64;
        let change = today - 3;
        let offset_at = move |unix: i64| {
            if unix < change * 86_400 + 7 * 3600 {
                5 * 3600
            } else {
                4 * 3600
            }
        };
        let day_start = |day: i64| {
            let offset = if day <= change { 5 * 3600 } else { 4 * 3600 };
            day * 86_400 - offset
        };
        let now_ms = (day_start(today) + 15 * 3600) * 1000;
        let row = |turn, day: i64, extra_ms: i64, input| {
            usage_row(
                turn,
                (day_start(day) + extra_ms / 1000) * 1000 + extra_ms % 1000,
                Provider::Claude,
                "a",
                tokens(input, 0, 0),
                None,
            )
        };
        save(
            &store,
            "t1",
            vec![
                row(0, change, 0, 1),
                row(1, change + 1, -1, 10),
                row(2, change + 1, 0, 100),
                row(3, today - 6, -1, 1000),
            ],
        );
        let days = store.daily_tokens(now_ms, 7, offset_at).unwrap();
        let claude: Vec<_> = days
            .iter()
            .filter(|day| day.provider == Provider::Claude)
            .map(|day| (day.day_start_unix_seconds, day.tokens.input))
            .collect();
        let expected: Vec<_> = (today - 6..=today)
            .map(|day| {
                let input = match day {
                    d if d == change => 11,
                    d if d == change + 1 => 100,
                    _ => 0,
                };
                (day_start(day), input)
            })
            .collect();
        assert_eq!(claude, expected);
        assert_eq!(
            claude[4].0 - claude[3].0,
            25 * 3600,
            "the clocks went back that day, so it is 25 hours long"
        );
    }

    #[test]
    fn the_local_offset_is_a_whole_number_of_minutes_within_a_day() {
        let offset = local_utc_offset_seconds(1_800_000_000);
        assert!(offset.abs() <= 14 * 3600 && offset % 60 == 0, "{offset}");
    }
}
