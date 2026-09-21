//! Requires a live, empty PostgreSQL database named by `HIVE_TEST_DATABASE_URL`
//! (falls back to `postgres://hive:hive@127.0.0.1:15432/hive`). Not run by default: `cargo test`
//! skips
//! `#[ignore]` tests. Run explicitly with:
//!   cargo test -p hive-persistence --test migrator_integration -- --ignored
//!
//! This guards the migrator's dialect probe. Rewriting `CREATE INDEX` to `CREATE INDEX ASYNC`,
//! and check-constraint validation to `ALTER TABLE ASYNC ... VALIDATE CONSTRAINT`, produces
//! syntax plain PostgreSQL rejects, so the probe must keep those rewrites to Aurora DSQL. This
//! test proves the unmodified files under `db/migration/` apply against PostgreSQL.

use hive_persistence::entity::{hive_schema_migrations, principals};
use sea_orm::{Database, EntityTrait, PaginatorTrait};

fn test_database_url() -> String {
    std::env::var("HIVE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://hive:hive@127.0.0.1:15432/hive".to_string())
}

#[tokio::test]
#[ignore]
async fn migrates_a_fresh_postgres_database_with_no_schema_edits() {
    let mut migrator_options = sea_orm::ConnectOptions::new(test_database_url());
    migrator_options.max_connections(1);
    let db = Database::connect(migrator_options)
        .await
        .expect("connect the migrator to the test database");

    hive_persistence::migrate_and_seed(&db)
        .await
        .expect("first migration run must succeed against a fresh database");

    let ledger_count = hive_schema_migrations::Entity::find()
        .count(&db)
        .await
        .unwrap();
    // 3 seed markers (S001, S009, S011) + 27 ledgered migrations (V015-V038 is 24
    // files, plus V016_1, V039, V040).
    assert_eq!(ledger_count, 30);

    let seeded_principal = principals::Entity::find_by_id(
        uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
    )
    .one(&db)
    .await
    .unwrap()
    .expect("the organization directory seed inserts principal 00000000-...-0001");
    assert_eq!(seeded_principal.display_name, "Ada Lovelace");

    hive_persistence::migrate_and_seed(&db)
        .await
        .expect("a second run against an already-migrated database must be a no-op, not an error");

    let ledger_count_after_replay = hive_schema_migrations::Entity::find()
        .count(&db)
        .await
        .unwrap();
    assert_eq!(
        ledger_count_after_replay, 30,
        "the unconditional V000-V014 and V039-repair steps must not insert duplicate ledger rows"
    );
}
