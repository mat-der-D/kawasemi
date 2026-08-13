//! Counting the SQL statements a piece of code actually issues.
//!
//! Prose has already proven to be the wrong place to keep query counts — a
//! written tally of `assemble_many`'s queries drifted twice while the
//! batching was being built. So the counts live here instead, as something
//! measured rather than asserted in a comment: the tests that use this module
//! are the first-class record of how many queries a list render costs, and
//! they go red when that changes.
//!
//! ## How the counting works
//! `sqlx` already emits one `tracing` event per executed statement, on the
//! `sqlx::query` target, carrying the statement text (`db.statement`, or
//! `summary` alone for statements short enough that the summary *is* the
//! statement — see `sqlx_core::logger::QueryLogger::finish`). Counting those
//! events counts executions, one-for-one, with no wrapper between the code
//! under test and the pool it really uses: every test here runs the same
//! production repository functions against the same real PostgreSQL
//! connection the rest of the suite uses.
//!
//! [`record_queries`] attaches a capturing subscriber to **one future** via
//! [`WithSubscriber`], not to the process and not to the thread. That is the
//! property that makes this usable at all in this crate's test suite: this
//! suite runs many `#[tokio::test]`s concurrently against one shared
//! database, so any counter living in the database (`pg_stat_statements`,
//! `pg_stat_database`) or in a process-global subscriber would count other
//! tests' queries as well as its own. A per-future dispatcher counts exactly
//! the statements issued while the measured future is being polled, and
//! nothing else. `tracing::subscriber::set_default` — the pattern
//! `server/tests.rs` and `telemetry/tests.rs` use — would be *nearly*
//! equivalent here, but it scopes to the thread rather than to the task, so
//! it would silently stop being correct if a measured path were ever polled
//! on a multi-threaded runtime.
//!
//! The subscriber carries no `EnvFilter`, which is deliberate:
//! `LogConfig::sql_diagnostic` reaches the `sqlx::query` target through
//! [`crate::telemetry`]'s filter on the *application's* global subscriber,
//! and a measurement a configuration flag could switch off would be worse
//! than no measurement at all. `sqlx` logs at its own default `DEBUG`
//! (nothing in this crate calls `log_statements`), so what arrives here is
//! every statement the pool executed.
//!
//! ## Why the measurement starts with a warm-up query
//! `tracing` caches, per callsite, whether *anyone* is interested in it, and
//! computes that cache the first time the callsite is reached — against
//! whichever dispatcher happens to be the calling thread's default at that
//! instant. In a suite where several `#[tokio::test]`s run at once, the
//! thread that first reaches `sqlx`'s statement callsite is very often a
//! test that is *not* measuring, whose default is `NoSubscriber`; the
//! callsite is then cached as "never", every subsequent measurement on every
//! thread silently records zero, and the tests using it pass or fail by
//! coin-flip. That was observed, not hypothesized — it cost two runs in six
//! before this was addressed.
//!
//! [`record_queries`] therefore opens every measurement, inside the
//! dispatcher's scope, by repeating two things until a throwaway statement
//! of its own comes back captured: rebuild the interest cache — which,
//! evaluated here, resolves every callsite against a dispatcher that *does*
//! want the events, repairing whatever a non-measuring thread cached — and
//! execute the throwaway statement, which forces any still-unregistered
//! `sqlx` callsite to be registered under that same dispatcher. It loops
//! because a callsite can only be poisoned by its *first* registration, and
//! `sqlx`'s statement logging has two of them (the `enabled!` gate, then the
//! event), so a concurrent thread can win at most one round each. After
//! that, the only thing that recomputes a registered callsite is
//! `register_dispatch`, whose dispatcher set is exactly the live
//! [`QueryLog`]s — so a capture, once obtained, cannot be taken away.
//!
//! The warm-up statement is *asserted* to have been captured rather than
//! hoped for: a mechanism that has stopped recording fails here, loudly and
//! at the right place, instead of turning into a mystery "issued none"
//! assertion in whichever test happened to be measuring. It is cleared
//! afterwards, so it counts toward nothing.
//!
//! ## What the counts do and do not include
//! Only executed statements. Acquiring a pooled connection, and `sqlx`'s
//! own pre-acquire liveness check, issue no statement and so appear nowhere;
//! statements issued inside a transaction, including its `BEGIN`/`COMMIT`,
//! appear like any other. Statements are normalized (collapsed whitespace)
//! so that the same SQL literal always produces the same key, and are keyed
//! by their full text — which is what lets two queries against the same
//! table be told apart, e.g. the batched
//! `emoji_repository::resolve_emojis` (`WHERE shortcode = ANY($1)`) from
//! `list_visible_emojis`, which `AccountService::show_account` issues once
//! per account it renders.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// The `tracing` target `sqlx` logs every executed statement on.
const SQLX_QUERY_TARGET: &str = "sqlx::query";

/// `sqlx`'s field carrying the full statement text — empty when the
/// statement is short enough that [`SUMMARY_FIELD`] already holds all of it.
const STATEMENT_FIELD: &str = "db.statement";

/// `sqlx`'s field carrying the statement's first four words.
const SUMMARY_FIELD: &str = "summary";

/// What a captured statement is counted as.
///
/// The five per-status material kinds a list render batches — media, tags,
/// emoji, interaction state, polls — plus account resolution, so a test can
/// assert on the thing it means rather than on a SQL string.
///
/// Classification is by full statement text, so a batched lookup and a
/// per-row one against the same table are different kinds — that
/// distinction is the whole point of measuring here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueryKind {
    /// Batched: `status_media` rows, and the `media` rows they point at.
    Media,
    /// Per status: one `status_media` lookup for a single `status_id`.
    /// `account_provider`'s `only_media` filter still issues these — an
    /// accepted residual of the batching.
    MediaPerStatus,
    /// Batched: a page's tags.
    Tags,
    /// Batched: the custom emoji named by a page's content and poll options.
    Emoji,
    /// Batched: the viewer's favourite/reblog/bookmark/pin state.
    Interaction,
    /// Per status: a single `pins` existence check. `account_provider`'s
    /// `pinned` filter still issues these — an accepted residual.
    InteractionPerStatus,
    /// Batched: `polls`, `poll_options`, `poll_votes`.
    Poll,
    /// Per poll: the single-poll `polls`/`poll_options`/`poll_votes`
    /// lookups `PollService::poll` issues. `statuses/endpoints.rs`'s
    /// `PollServiceResolver` still issues these — an accepted residual.
    PollPerPoll,
    /// Per distinct account: what one `AccountService::show_account` costs.
    AccountResolution,
    /// Anything else — the caller's own row fetches, visibility lookups,
    /// transaction control, and the test's own fixture writes.
    Other,
}

impl QueryKind {
    /// Classifies one normalized statement.
    ///
    /// Written as ordered `if`s rather than a match on table name because
    /// several kinds share a table and are told apart only by the shape of
    /// their `WHERE` clause (`= ANY($n)` for a batched lookup against `= $n`
    /// for a per-row one).
    fn classify(sql: &str) -> Self {
        let batched = sql.contains("= ANY(");

        if sql.contains("FROM status_media") {
            return if batched {
                Self::Media
            } else {
                Self::MediaPerStatus
            };
        }
        if sql.contains("FROM media WHERE id = ANY(") {
            return Self::Media;
        }
        if sql.contains("INNER JOIN status_tags") {
            return Self::Tags;
        }
        if sql.contains("FROM custom_emojis WHERE shortcode = ANY(") {
            return Self::Emoji;
        }
        if sql.contains("FROM poll_options") || sql.contains("FROM poll_votes") {
            return if batched {
                Self::Poll
            } else {
                Self::PollPerPoll
            };
        }
        if sql.contains("FROM polls") {
            return if batched {
                Self::Poll
            } else {
                Self::PollPerPoll
            };
        }
        if sql.contains("FROM favourites") || sql.contains("FROM bookmarks") {
            return if batched {
                Self::Interaction
            } else {
                Self::Other
            };
        }
        if sql.contains("FROM pins") {
            return if batched {
                Self::Interaction
            } else {
                Self::InteractionPerStatus
            };
        }
        if sql.contains("FROM statuses WHERE actor_id = $1 AND reblog_of_id = ANY(") {
            return Self::Interaction;
        }
        if is_account_resolution(sql) {
            return Self::AccountResolution;
        }
        Self::Other
    }
}

/// Whether `sql` is one of the statements `AccountService::show_account`
/// issues for a local author.
///
/// Deliberately the whole set rather than one representative statement: an
/// author resolved twice costs every one of these twice, and counting all of
/// them means the "proportional to the number of distinct authors"
/// assertion fails on a regression that duplicated any
/// part of the resolution, not only on one that duplicated the actor lookup.
fn is_account_resolution(sql: &str) -> bool {
    (sql.contains("FROM local_actors") && !sql.contains("= ANY("))
        || sql.contains("FROM account_profiles")
        || (sql.contains("FROM custom_emojis") && sql.contains("visible_in_picker"))
        || sql.contains("FROM remote_accounts")
}

/// Every statement executed while a measured future was being polled, in
/// execution order.
#[derive(Clone, Default)]
pub(crate) struct QueryLog {
    statements: Arc<Mutex<Vec<String>>>,
}

impl QueryLog {
    /// Every captured statement, normalized, in execution order.
    pub(crate) fn statements(&self) -> Vec<String> {
        self.statements
            .lock()
            .expect("the query log mutex is never poisoned: nothing panics while holding it")
            .clone()
    }

    /// Drops everything captured so far.
    fn clear(&self) {
        self.statements
            .lock()
            .expect("the query log mutex is never poisoned: nothing panics while holding it")
            .clear();
    }

    /// How many times each distinct statement ran.
    ///
    /// `BTreeMap` so that an assertion failure prints the two sides in the
    /// same, stable order and the offending statement is findable by eye.
    pub(crate) fn per_statement(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for statement in self.statements() {
            *counts.entry(statement).or_insert(0) += 1;
        }
        counts
    }

    /// How many statements of each [`QueryKind`] ran.
    ///
    /// Kinds with no statements are absent rather than zero, so a test that
    /// means "this kind was exercised" has to say so — see
    /// [`QueryLog::require_kinds`].
    pub(crate) fn per_kind(&self) -> BTreeMap<QueryKind, usize> {
        let mut counts = BTreeMap::new();
        for statement in self.statements() {
            *counts.entry(QueryKind::classify(&statement)).or_insert(0) += 1;
        }
        counts
    }

    /// How many statements of `kind` ran.
    pub(crate) fn count(&self, kind: QueryKind) -> usize {
        self.per_kind().get(&kind).copied().unwrap_or(0)
    }

    /// How many captured statements contain `needle`.
    ///
    /// For pinning one specific lookup that no [`QueryKind`] names — a
    /// residual a test wants on the record rather than a category the
    /// requirements enumerate.
    pub(crate) fn count_matching(&self, needle: &str) -> usize {
        self.statements()
            .iter()
            .filter(|sql| sql.contains(needle))
            .count()
    }

    /// Panics unless every kind in `kinds` was actually exercised.
    ///
    /// The guard against a vacuous count-independence assertion: two runs
    /// that both issue zero poll queries have trivially equal poll counts,
    /// and would keep having them after the batching was torn out.
    pub(crate) fn require_kinds(&self, kinds: &[QueryKind]) {
        let observed = self.per_kind();
        for kind in kinds {
            assert!(
                observed.get(kind).copied().unwrap_or(0) > 0,
                "expected the measured call to issue at least one {kind:?} query, \
                 but it issued none — the fixture is not exercising it, so any \
                 count-independence assertion about it would pass vacuously. \
                 Observed: {observed:?}"
            );
        }
    }
}

impl<S> Layer<S> for QueryLog
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != SQLX_QUERY_TARGET {
            return;
        }
        let mut visitor = StatementVisitor::default();
        event.record(&mut visitor);
        if let Some(statement) = visitor.into_statement() {
            self.statements
                .lock()
                .expect("the query log mutex is never poisoned: nothing panics while holding it")
                .push(statement);
        }
    }
}

/// Pulls the statement text out of one `sqlx::query` event.
#[derive(Default)]
struct StatementVisitor {
    statement: Option<String>,
    summary: Option<String>,
}

impl StatementVisitor {
    /// The full statement when `sqlx` recorded one, else the summary it
    /// records instead for a statement of four words or fewer.
    fn into_statement(self) -> Option<String> {
        let statement = self
            .statement
            .filter(|sql| !sql.trim().is_empty())
            .or(self.summary)?;
        Some(normalize(&statement))
    }
}

impl Visit for StatementVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            STATEMENT_FIELD => self.statement = Some(value.to_string()),
            SUMMARY_FIELD => self.summary = Some(value.to_string()),
            _ => {}
        }
    }

    /// Fallback for the day `sqlx` records either field as something other
    /// than a string. Losing the statement silently would turn every count
    /// here into zero, and zero compares equal to zero — so a Debug-rendered
    /// key, imperfect as it is, is preferable to no key.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            STATEMENT_FIELD if self.statement.is_none() => {
                self.statement = Some(format!("{value:?}"));
            }
            SUMMARY_FIELD if self.summary.is_none() => {
                self.summary = Some(format!("{value:?}"));
            }
            _ => {}
        }
    }
}

/// Collapses every run of whitespace to one space and trims, so that the
/// same SQL literal always produces the same key regardless of how it was
/// wrapped in the source.
fn normalize(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One throwaway statement, executed under the capturing dispatcher before
/// anything is measured. See this module's doc comment, "Why the measurement
/// starts with a warm-up query".
const WARMUP_SQL: &str = "SELECT 1 AS query_log_warmup";

/// The part of [`WARMUP_SQL`] that identifies it in a capture. `sqlx` records
/// only a four-word summary for a statement this short, so the marker has to
/// be inside the first four words.
const WARMUP_MARKER: &str = "query_log_warmup";

/// How many times [`record_queries`] will repair the interest cache and
/// retry its warm-up before giving up.
///
/// Each round can lose to at most one *first* registration of one callsite
/// by a non-measuring thread, and `sqlx`'s statement logging has two of them
/// (the `enabled!` gate and the event itself), so two rounds suffice and the
/// rest is margin. Once a callsite is registered, only `register_dispatch`
/// recomputes it, and every registered dispatcher is a live [`QueryLog`] —
/// so a round that captures cannot be undone by a later one.
const WARMUP_ATTEMPTS: usize = 8;

/// Runs `fut` to completion, counting every SQL statement executed while it
/// was being polled.
///
/// The capture is scoped to this future alone — see this module's doc
/// comment — so concurrently running tests never contribute to the count.
/// `pool` is the pool the measured code reads through; it is used only for
/// the warm-up statement that makes the capture reliable under concurrency.
///
/// # Panics
/// If the warm-up statement is not captured, i.e. if the mechanism is not
/// actually recording. Failing here rather than returning an empty log keeps
/// a broken measurement from being read as "this code issues no queries".
pub(crate) async fn record_queries<F: Future>(
    pool: &sqlx::PgPool,
    fut: F,
) -> (F::Output, QueryLog) {
    let log = QueryLog::default();
    let subscriber = tracing_subscriber::registry().with(log.clone());
    let warmup = log.clone();

    let output = async move {
        let mut warmed = false;
        for _ in 0..WARMUP_ATTEMPTS {
            // Repairs a callsite a non-measuring thread cached as "never".
            // Run inside this dispatcher's scope, which is what makes the
            // rebuild resolve to "always" rather than to whatever the other
            // thread saw.
            tracing::callsite::rebuild_interest_cache();
            warmup.clear();
            sqlx::query(WARMUP_SQL)
                .execute(pool)
                .await
                .expect("the warm-up statement must execute");
            if warmup.count_matching(WARMUP_MARKER) > 0 {
                warmed = true;
                break;
            }
        }
        assert!(
            warmed,
            "the query log captured nothing for its own warm-up statement after \
             {WARMUP_ATTEMPTS} attempts, so it would have recorded zero for \
             everything else too"
        );
        warmup.clear();
        fut.await
    }
    .with_subscriber(subscriber)
    .await;

    (output, log)
}
