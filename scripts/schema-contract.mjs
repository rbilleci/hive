import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { buildSchema } from "graphql";

// The console compiles its cynic operations against schema/hive.graphql, so that file must be the
// SDL the service actually serves, and must be valid standalone GraphQL.
const binary = join(process.cwd(), "target", "release", "hive");
assert.ok(existsSync(binary), "Build the release binary first: cargo build --release");
const runtimeSdl = execFileSync(binary, ["schema-sdl"], { encoding: "utf8" });
assert.equal(readFileSync("schema/hive.graphql", "utf8"), runtimeSdl, "schema/hive.graphql is stale; run npm run generate:schema.");
const schema = buildSchema(runtimeSdl);
const queries = Object.keys(schema.getQueryType().getFields()).length;
const mutations = Object.keys(schema.getMutationType().getFields()).length;
console.log(`schema: schema/hive.graphql equals the served SDL and parses (${queries} query fields, ${mutations} mutation fields).`);
