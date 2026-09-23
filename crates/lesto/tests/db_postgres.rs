//! End to end on Postgres: a principal's [`TransactionSettings`] reach a row level security
//! policy, and stop existing when the transaction ends.
//!
//! Needs a server, so it is the one test in the suite that is skipped rather than run when its
//! environment is missing:
//!
//! ```sh
//! docker run --rm -e POSTGRES_PASSWORD=lesto -p 5432:5432 postgres:18
//! LESTO_TEST_POSTGRES_URL=postgres://postgres:lesto@127.0.0.1:5432/postgres \
//!   cargo test -p lesto --test db_postgres
//! ```
//!
//! Without `LESTO_TEST_POSTGRES_URL` each test prints a line and passes: the rendering of the
//! statement is covered without a server by the unit tests in `src/db/dialect.rs`, and this
//! file exists to check the half those cannot — that Postgres agrees.

use lesto::db::prelude::*;
use lesto::db::uuid::Uuid;
use lesto::http::StatusCode;
use lesto::prelude::*;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Pool, Postgres, Row};

// ---- principal ------------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    db: Db<Postgres>,
}

impl lesto::axum::extract::FromRef<AppState> for Db<Postgres> {
    fn from_ref(s: &AppState) -> Self {
        s.db.clone()
    }
}

/// The identity the RLS policies below read out of the transaction.
#[derive(Debug)]
struct Tenant {
    id: Uuid,
    is_staff: bool,
}

impl Authenticated for Tenant {
    type State = AppState;
    type Credential = Bearer;

    async fn authenticate(_token: Bearer, _state: &AppState) -> Result<Self, HttpError> {
        unreachable!("this test builds its stores by hand")
    }

    fn has_permission(&self, _permission: &str) -> bool {
        true
    }

    fn transaction_settings(&self) -> TransactionSettings {
        TransactionSettings::empty()
            .set("app.user_id", self.id)
            .set("app.is_staff", self.is_staff)
    }
}

// ---- store ----------------------------------------------------------------------------------

#[derive(lesto::db::Store)]
struct NoteStore<M, P>(Store<M, P, Postgres>);

impl<M: Mode, P> NoteStore<M, P> {
    /// What the caller can see. No `WHERE` clause: the policy is the filter.
    async fn list(&self) -> Result<Vec<(Uuid, String)>, Error> {
        self.read(Anyone, async |conn| {
            let rows = sqlx::query("SELECT owner, text FROM notes ORDER BY text")
                .fetch_all(conn)
                .await?;
            Ok::<_, Error>(
                rows.into_iter()
                    .map(|r| (r.get::<Uuid, _>("owner"), r.get::<String, _>("text")))
                    .collect(),
            )
        })
        .await
    }

    /// The identity as the database sees it, straight out of the settings.
    async fn whoami(&self) -> Result<(Option<String>, Option<String>), Error> {
        self.read(Anyone, async |conn| {
            let row = sqlx::query(
                "SELECT nullif(current_setting('app.user_id', true), '') AS user_id, \
                        nullif(current_setting('app.is_staff', true), '') AS is_staff",
            )
            .fetch_one(conn)
            .await?;
            Ok::<_, Error>((row.get("user_id"), row.get("is_staff")))
        })
        .await
    }
}

impl<M: Writable, P> NoteStore<M, P> {
    async fn insert(&self, owner: Uuid, text: &str) -> Result<u64, Error> {
        self.write(Anyone, async |conn| {
            let done = sqlx::query("INSERT INTO notes (owner, text) VALUES ($1, $2)")
                .bind(owner)
                .bind(text)
                .execute(conn)
                .await?;
            Ok::<_, Error>(done.rows_affected())
        })
        .await
    }
}

// ---- fixtures -------------------------------------------------------------------------------

const ALICE: Uuid = Uuid::from_u128(0xa11ce);
const BOB: Uuid = Uuid::from_u128(0xb0b);

/// The role the tests act as.
///
/// Not the one in the URL: `postgres` is a superuser, and a superuser bypasses row level
/// security outright, so a test running as it would pass no matter what the policies say.
/// Every pooled connection does `SET ROLE` to this one, which has neither `SUPERUSER` nor
/// `BYPASSRLS`, and is therefore subject to the policies.
const APP_ROLE: &str = "lesto_rls_test";

/// Connect with `SET ROLE` and a `search_path` pinned to `schema`, so tests that run in
/// parallel do not fight over one `notes` table.
async fn connect(url: &str, schema: &str, max_connections: u32) -> Pool<Postgres> {
    let preamble = format!("SET ROLE {APP_ROLE}; SET search_path TO {schema}");
    PgPoolOptions::new()
        .max_connections(max_connections)
        .after_connect(move |conn, _| {
            let preamble = preamble.clone();
            Box::pin(async move {
                sqlx::raw_sql(sqlx::AssertSqlSafe(preamble))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .expect("LESTO_TEST_POSTGRES_URL is set but the server did not accept the connection")
}

/// The table, the policies and the seed rows, in a schema of this test's own.
///
/// `None` when `LESTO_TEST_POSTGRES_URL` is unset.
async fn setup(schema: &str) -> Option<(String, Pool<Postgres>)> {
    let url = std::env::var("LESTO_TEST_POSTGRES_URL").ok()?;

    // As the owner: build the schema, then hand the app role just enough to use it.
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("LESTO_TEST_POSTGRES_URL is set but the server did not accept the connection");
    for stmt in [
        // Tests run in parallel and all want this role. Losing the race is not a failure —
        // and it surfaces as `unique_violation` on the catalog index, not only as
        // `duplicate_object`.
        format!(
            "DO $$ BEGIN CREATE ROLE {APP_ROLE} NOSUPERUSER NOBYPASSRLS;              EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
        ),
        format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
        format!("CREATE SCHEMA {schema}"),
        format!("GRANT USAGE ON SCHEMA {schema} TO {APP_ROLE}"),
        format!("CREATE TABLE {schema}.notes (owner uuid NOT NULL, text text NOT NULL)"),
        format!("GRANT SELECT, INSERT ON {schema}.notes TO {APP_ROLE}"),
        format!("ALTER TABLE {schema}.notes ENABLE ROW LEVEL SECURITY"),
        format!("ALTER TABLE {schema}.notes FORCE ROW LEVEL SECURITY"),
        // Read your own notes; staff read everything.
        format!(
            "CREATE POLICY notes_read ON {schema}.notes FOR SELECT USING (
                 owner = nullif(current_setting('app.user_id', true), '')::uuid
                 OR coalesce(nullif(current_setting('app.is_staff', true), '')::boolean, false)
             )"
        ),
        // Write only as yourself.
        format!(
            "CREATE POLICY notes_insert ON {schema}.notes FOR INSERT WITH CHECK (
                 owner = nullif(current_setting('app.user_id', true), '')::uuid
             )"
        ),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(stmt.clone()))
            .execute(&owner)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    for (owner_id, text) in [(ALICE, "alice-1"), (ALICE, "alice-2"), (BOB, "bob-1")] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {schema}.notes (owner, text) VALUES ($1, $2)"
        )))
        .bind(owner_id)
        .bind(text)
        .execute(&owner)
        .await
        .unwrap();
    }
    owner.close().await;

    Some((url.clone(), connect(&url, schema, 2).await))
}

/// Set up this test's schema, or say the test is skipped instead of passing in silence.
macro_rules! setup_or_skip {
    ($schema:expr) => {
        match setup($schema).await {
            Some(pair) => pair,
            None => {
                eprintln!("skipped: set LESTO_TEST_POSTGRES_URL to run the Postgres tests");
                return;
            }
        }
    };
}

fn store<M: Mode>(pool: &Pool<Postgres>, id: Uuid, is_staff: bool) -> NoteStore<M, Tenant> {
    NoteStore(Store::new(Db::new(pool.clone()), Tenant { id, is_staff }))
}

/// A store whose conflicting transactions get `retries` extra attempts.
fn store_retrying<M: Mode>(pool: &Pool<Postgres>, retries: u8) -> NoteStore<M, Tenant> {
    NoteStore(Store::new(
        Db::new(pool.clone()).with_conflict_retries(retries),
        Tenant {
            id: ALICE,
            is_staff: false,
        },
    ))
}

// ---- tests ----------------------------------------------------------------------------------

#[tokio::test]
async fn the_settings_reach_the_transaction() {
    let (_url, pool) = setup_or_skip!("lesto_rls_reach");
    let store = store::<ReadOnly>(&pool, ALICE, false);

    let (user_id, is_staff) = store.whoami().await.unwrap();
    assert_eq!(user_id.as_deref(), Some(ALICE.to_string().as_str()));
    assert_eq!(is_staff.as_deref(), Some("false"));
}

#[tokio::test]
async fn a_policy_filters_reads_by_the_published_identity() {
    let (_url, pool) = setup_or_skip!("lesto_rls_reads");

    let alice = store::<ReadOnly>(&pool, ALICE, false).list().await.unwrap();
    assert_eq!(
        alice
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        ["alice-1", "alice-2"]
    );

    let bob = store::<ReadOnly>(&pool, BOB, false).list().await.unwrap();
    assert_eq!(bob.len(), 1);
    assert_eq!(bob[0].1, "bob-1");

    let staff = store::<ReadOnly>(&pool, ALICE, true).list().await.unwrap();
    assert_eq!(staff.len(), 3, "staff sees every row");
}

#[tokio::test]
async fn a_policy_filters_writes_by_the_published_identity() {
    let (_url, pool) = setup_or_skip!("lesto_rls_writes");

    let alice = store::<ReadWrite>(&pool, ALICE, false);
    assert_eq!(alice.insert(ALICE, "alice-3").await.unwrap(), 1);

    // Writing as somebody else fails the `WITH CHECK`, which is a 500: a policy violation is a
    // bug in the query, not something a client chose.
    let error = alice.insert(BOB, "forged").await.unwrap_err();
    assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // And nothing was written.
    let bob = store::<ReadOnly>(&pool, BOB, false).list().await.unwrap();
    assert_eq!(bob.len(), 1);
}

#[tokio::test]
async fn a_read_only_store_cannot_write_even_with_the_identity_set() {
    let (_url, pool) = setup_or_skip!("lesto_rls_read_only");
    // `BEGIN READ ONLY` is still the first statement of the preamble, so Postgres refuses the
    // write on its own — the compile-time half of this is `M: Writable` on `insert`.
    let store = store::<ReadOnly>(&pool, ALICE, false);
    let error = store
        .read(Anyone, async |conn| {
            sqlx::query("INSERT INTO notes (owner, text) VALUES ($1, 'nope')")
                .bind(ALICE)
                .execute(conn)
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn the_settings_do_not_outlive_the_transaction() {
    let (url, pool) = setup_or_skip!("lesto_rls_lifetime");
    // One connection, so the second store is guaranteed to get the first one back.
    let single = connect(&url, "lesto_rls_lifetime", 1).await;

    let identified = store::<ReadOnly>(&single, ALICE, true);
    assert!(identified.whoami().await.unwrap().0.is_some());

    // `SET LOCAL` ends with the transaction: a `Public` store on the same connection sees
    // nothing, so a leaked identity cannot turn an anonymous request into an authorized one.
    let anonymous = NoteStore(Store::<ReadOnly, _, _>::new(Db::new(single), Public));
    let (user_id, is_staff) = anonymous.whoami().await.unwrap();
    assert_eq!(user_id, None);
    assert_eq!(is_staff, None);
    assert_eq!(
        anonymous.list().await.unwrap().len(),
        0,
        "policy denies all"
    );

    drop(pool);
}

// ---- isolation ------------------------------------------------------------------------------

/// Both transactions count Alice's notes, then both insert one. Under `SERIALIZABLE` that is a
/// read-write dependency Postgres refuses to serialize, so exactly one of them loses.
///
/// Retrying is switched off here: this is the test for the answer a conflict gets once the
/// attempts are spent. `a_conflict_is_retried_until_it_succeeds` covers the other half.
#[tokio::test]
async fn a_serializable_conflict_answers_409_and_not_500() {
    let (_url, pool) = setup_or_skip!("lesto_rls_conflict");
    // Both must read before either writes, or there is no conflict to detect. Safe as a plain
    // barrier only because no attempt is ever repeated: see the retrying test below.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

    let contend = |pool: Pool<Postgres>, barrier: std::sync::Arc<tokio::sync::Barrier>| async move {
        store_retrying::<ReadWrite>(&pool, 0)
            .write_with(Anyone, Isolation::Serializable, async |conn| {
                let counted: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM notes WHERE owner = $1")
                        .bind(ALICE)
                        .fetch_one(&mut *conn)
                        .await?;
                barrier.wait().await;
                sqlx::query("INSERT INTO notes (owner, text) VALUES ($1, $2)")
                    .bind(ALICE)
                    .bind(format!("counted-{counted}"))
                    .execute(conn)
                    .await?;
                Ok::<_, Error>(())
            })
            .await
    };

    let (first, second) = tokio::join!(
        contend(pool.clone(), barrier.clone()),
        contend(pool.clone(), barrier.clone())
    );

    let losers: Vec<Error> = [first, second]
        .into_iter()
        .filter_map(Result::err)
        .collect();
    assert_eq!(losers.len(), 1, "exactly one transaction should lose");

    let error = losers.into_iter().next().unwrap();
    assert_eq!(error.status(), StatusCode::CONFLICT, "{error}");
    assert!(error.is_transient_conflict());
    let problem = error.into_http_error();
    assert_eq!(
        problem
            .headers()
            .and_then(|h| h.get(lesto::http::header::RETRY_AFTER))
            .map(|v| v.to_str().unwrap()),
        Some("0")
    );
    assert!(problem.detail().contains("retry"), "{}", problem.detail());
}

/// The same contention, with the retry budget left on: the loser re-runs its closure, this time
/// alone, and both callers get an `Ok`.
#[tokio::test]
async fn a_conflict_is_retried_until_it_succeeds() {
    let (_url, pool) = setup_or_skip!("lesto_rls_retry");
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    // Every entry into a closure, retries included. Two conflicting transactions plus one
    // retry is three, at least: when the winner has not committed yet by the time the retry
    // reads, the two conflict again and one more retry follows (seen on CI runners).
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let contend = |pool: Pool<Postgres>,
                   barrier: std::sync::Arc<tokio::sync::Barrier>,
                   attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>| async move {
        // Only the first attempt waits: a retried closure has nobody left to meet at the
        // barrier, and waiting again would hang.
        let waited = std::sync::atomic::AtomicBool::new(false);
        store_retrying::<ReadWrite>(&pool, 2)
            .write_with(Anyone, Isolation::Serializable, async |conn| {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let counted: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM notes WHERE owner = $1")
                        .bind(ALICE)
                        .fetch_one(&mut *conn)
                        .await?;
                if !waited.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    barrier.wait().await;
                }
                sqlx::query("INSERT INTO notes (owner, text) VALUES ($1, $2)")
                    .bind(ALICE)
                    .bind(format!("retried-{counted}"))
                    .execute(conn)
                    .await?;
                Ok::<_, Error>(())
            })
            .await
    };

    let (first, second) = tokio::join!(
        contend(pool.clone(), barrier.clone(), attempts.clone()),
        contend(pool.clone(), barrier.clone(), attempts.clone())
    );
    assert!(first.is_ok(), "{first:?}");
    assert!(second.is_ok(), "{second:?}");
    let attempts = attempts.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        attempts >= 3,
        "two first attempts plus at least one retry, got {attempts}"
    );

    // Both rows landed: the retry committed, it did not silently swallow the write.
    let rows = store::<ReadOnly>(&pool, ALICE, false).list().await.unwrap();
    assert_eq!(rows.len(), 4, "two seeded plus two inserted");
}

/// The isolation clause and the transaction settings coexist: the identity still reaches the
/// policy when the transaction is serializable.
#[tokio::test]
async fn settings_still_reach_a_serializable_transaction() {
    let (_url, pool) = setup_or_skip!("lesto_rls_serializable");
    let visible = store::<ReadOnly>(&pool, ALICE, false)
        .read_with(Anyone, Isolation::Serializable, async |conn| {
            let rows = sqlx::query("SELECT text FROM notes")
                .fetch_all(conn)
                .await?;
            Ok::<_, Error>(rows.len())
        })
        .await
        .unwrap();
    assert_eq!(visible, 2, "the policy still filters to Alice's rows");
}
