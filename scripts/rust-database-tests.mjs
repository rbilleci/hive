import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { createIsolatedDatabase, postgresPort } from "./local-service.mjs";

// The database-backed Rust tests are #[ignore]d so `cargo test` needs no PostgreSQL. They share one
// database and start concurrently, so it is migrated once here rather than raced by every test.
const database = await createIsolatedDatabase("hive_rust_tests");
let status = 1;
try {
  const binary = process.env.HIVE_BINARY ?? join(process.cwd(), "target", "release", "hive");
  const migrate = spawnSync(binary, ["migrate"], {
    stdio: "inherit",
    env: { ...process.env, HIVE_DATABASE_URL: `jdbc:postgresql://127.0.0.1:${postgresPort()}/${database.name}`, HIVE_DATABASE_USER: "hive", HIVE_DATABASE_PASSWORD: "hive" }
  });
  if (migrate.status !== 0) throw new Error("The isolated test database could not be migrated.");
  const tests = spawnSync("cargo", ["test", "--workspace", "--", "--ignored"], {
    stdio: "inherit",
    env: { ...process.env, HIVE_TEST_DATABASE_URL: `postgres://hive:hive@127.0.0.1:${postgresPort()}/${database.name}` }
  });
  status = tests.status ?? 1;
} finally {
  await database.drop();
}
process.exit(status);
