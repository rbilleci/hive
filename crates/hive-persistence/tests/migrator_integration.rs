//! Requires a live, empty PostgreSQL database named by `HIVE_TEST_DATABASE_URL`
//! (falls back to `postgres://hive:hive@127.0.0.1:15432/hive`, the port this
//! session's scratch container used). Not run by default: `cargo test` skips
//! `#[ignore]` tests. Run explicitly with:
//!   cargo test -p hive-persistence --test migrator_integration -- --ignored
//!
//! This is the regression test for the finding that drove `RTD-MIGRATOR-PARITY`:
//! `DatabaseMigrator.java` has no dialect probe and unconditionally rewrites
//! `CREATE INDEX` to `CREATE INDEX ASYNC` and check-constraint validation to
//! `ALTER TABLE ASYNC ... VALIDATE CONSTRAINT`, syntax plain PostgreSQL rejects.
//! Every migration file under `db/migration/` is otherwise unmodified from the
//! Java service this repository was ported from; this test proves those files are
//! sufficient once the migrator is dialect-aware, with zero schema file changes.

use sea_orm::Database;
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

fn test_database_url() -> String {
    std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string())
}

#[tokio::test]
#[ignore]
async fn migrates_a_fresh_postgres_database_with_no_schema_edits() {
    let pool = PgPoolOptions::new()
        .connect(&test_database_url())
        .await
        .expect("connect to the test database");
    // The migrator speaks `DatabaseConnection` (`GSR-PHASE-1`); this test's own row assertions
    // below keep using the `sqlx` pool directly, since rewriting them is not this phase's concern.
    let mut migrator_options = sea_orm::ConnectOptions::new(test_database_url());
    migrator_options.max_connections(1);
    let db = Database::connect(migrator_options)
        .await
        .expect("connect the migrator to the test database");

    hive_persistence::migrate_and_seed(&db)
        .await
        .expect("first migration run must succeed against a fresh database");

    let ledger_count: i64 = sqlx::query("SELECT count(*) FROM hive_schema_migrations")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    // 3 seed markers (S001, S009, S011) + 27 ledgered migrations (V015-V038 is 24
    // files, plus V016_1, V039, V040).
    assert_eq!(ledger_count, 30);

    let seeded_principal: String = sqlx::query(
        "SELECT display_name FROM principals WHERE id = '00000000-0000-0000-0000-000000000001'",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(seeded_principal, "Ada Lovelace");

    hive_persistence::migrate_and_seed(&db)
        .await
        .expect("a second run against an already-migrated database must be a no-op, not an error");

    let ledger_count_after_replay: i64 = sqlx::query("SELECT count(*) FROM hive_schema_migrations")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        ledger_count_after_replay, 30,
        "the unconditional V000-V014 and V039-repair steps must not insert duplicate ledger rows"
    );
}
