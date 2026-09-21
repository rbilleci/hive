import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";

// The service builds, serves and validates from this tree alone: the console it serves and the
// schema that console compiles against are both produced here, and a checkout missing any of the
// paths below cannot build or start.
const findings = [];
for (const required of ["crates/hive-console/Cargo.toml", "crates/hive-console/index.html", "schema/hive.graphql", "db/migration", "db/seed", "infra/local/compose.yaml", "Dockerfile"]) {
  if (!existsSync(required)) findings.push(`${required}: required to build and run, and missing`);
}
assert.match(readFileSync("crates/hive-api/src/lib.rs", "utf8"), /env_string\("HIVE_WEB_DIST", "crates\/hive-console\/dist"\)/, "HIVE_WEB_DIST must default to the in-tree console build.");
assert.match(readFileSync("crates/hive-console/build.rs", "utf8"), /schema\/hive\.graphql/, "The console must check its operations against the in-tree schema.");

if (findings.length) {
  console.error(findings.join("\n"));
  console.error(`standalone-boundary: ${findings.length} finding(s).`);
  process.exit(1);
}
console.log("standalone-boundary: the console build and its schema are in-tree, and every path needed to build and run is present.");
