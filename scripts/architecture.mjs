import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

// Which crate may depend on which database or GraphQL framework, checked by source scan since
// Cargo alone does not enforce a *direction*, only that a dependency graph exists
// (docs/idiomatic-seaography-plan.md). `sqlx` is absent on purpose: no crate declares it, and it
// reaches the build only through sea-orm.
const allowedCrates = {
  "sea-orm": ["hive-persistence", "hive-api"],
  seaography: ["hive-persistence", "hive-api"],
  "async-graphql": ["hive-api"],
  axum: ["hive-api"]
};

const findings = [];
const crates = execFileSync("find", ["crates", "-maxdepth", "1", "-mindepth", "1", "-type", "d"], { encoding: "utf8" })
  .trim().split("\n").map((path) => path.split("/").pop()).filter(Boolean);

for (const [dependency, allowed] of Object.entries(allowedCrates)) {
  for (const crate of crates) {
    const manifest = `crates/${crate}/Cargo.toml`;
    let text;
    try { text = readFileSync(manifest, "utf8"); } catch { continue; }
    const declares = new RegExp(`^${dependency}(\\.workspace)?\\s*=`, "m").test(text);
    if (declares && !allowed.includes(crate)) {
      findings.push(`${manifest}: declares "${dependency}", which only ${allowed.join(", ")} may depend on`);
    }
  }
}

// `hive-api` composes the schema through SeaORM/Seaography's typed query builder only
// (`graphql_schema/tenant_hooks.rs` calls into `hive_persistence::entity::tenant`, which owns the
// one hand-built sea-query statement this phase has); it must never construct a sea-query
// statement or a raw SQL string directly.
const apiSources = execFileSync("find", ["crates/hive-api/src", "-name", "*.rs"], { encoding: "utf8" }).trim().split("\n");
for (const path of apiSources) {
  const text = readFileSync(path, "utf8");
  for (const pattern of [/sea_orm::sea_query::/, /\bsea_query::/, /Statement::from_sql/, /Statement::from_string/]) {
    if (pattern.test(text)) findings.push(`${path}: matches ${pattern}, but hive-api may not construct a sea-query statement directly`);
  }
}

if (findings.length) {
  console.error(findings.join("\n"));
  console.error(`architecture: ${findings.length} finding(s).`);
  process.exitCode = 1;
} else {
  console.log(`architecture: ${crates.length} crates, ${Object.keys(allowedCrates).length} dependency-direction rules, ${apiSources.length} hive-api source files — all conform.`);
}
