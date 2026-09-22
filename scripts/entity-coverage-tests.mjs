import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { createIsolatedDatabase, postgresPort } from "./local-service.mjs";

// The entity coverage test (`crates/hive-persistence/tests/entity_coverage.rs`) already
// runs as part of `check:rust:database`'s full `cargo test --workspace -- --ignored` sweep; this
// script runs just that one test file against its own freshly migrated database, so
// `check:schema:entity-coverage` and `check:schema:entity-relations` each report a focused pass or
// failure in `validate:local` instead of being buried inside the larger suite. Both names run the
// same test: column coverage and relation-column validity are checked in one pass per module
// (`entity_coverage.rs`'s own doc comment explains why splitting them would duplicate the same
// per-entity database round trips for no benefit while every relation is empty).
const database = await createIsolatedDatabase("hive_entity_coverage");
let status = 1;
try {
  const binary = process.env.HIVE_BINARY ?? join(process.cwd(), "target", "release", "hive");
  const migrate = spawnSync(binary, ["migrate"], {
    stdio: "inherit",
    env: { ...process.env, HIVE_DATABASE_URL: `jdbc:postgresql://127.0.0.1:${postgresPort()}/${database.name}`, HIVE_DATABASE_USER: "hive", HIVE_DATABASE_PASSWORD: "hive" }
  });
  if (migrate.status !== 0) throw new Error("The isolated entity-coverage database could not be migrated.");
  const tests = spawnSync("cargo", ["test", "-p", "hive-persistence", "--test", "entity_coverage", "--", "--ignored"], {
    stdio: "inherit",
    env: { ...process.env, HIVE_TEST_DATABASE_URL: `postgres://hive:hive@127.0.0.1:${postgresPort()}/${database.name}` }
  });
  status = tests.status ?? 1;
} finally {
  await database.drop();
}
process.exit(status);
