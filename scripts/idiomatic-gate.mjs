// Gate for docs/idiomatic-seaography-plan.md (G1-G7; G8 is validate:local itself).
// Report mode prints counts and exits 0. `--enforce` exits 1 on any violation.
// `--module <name>` enforces G1/G2 for one hive-persistence module only (a phase exit check).
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, relative } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const enforce = process.argv.includes("--enforce");
const moduleIndex = process.argv.indexOf("--module");
const onlyModule = moduleIndex > 0 ? process.argv[moduleIndex + 1] : null;

const RAW_SQL = [
  "from_sql_and_values", "Statement::from_string", "execute_unprepared", "query_one_raw",
  "query_all_raw", "execute_raw", "Expr::cust", "cust_with_values", "cust_with_exprs",
  "SimpleExpr::Custom", "raw_sql!", "from_raw_sql", "sqlx::query",
];
const HAND_BUILT = ["register_custom_query", "Field::new(", "Object::new(", "InputObject::new(", "Interface::new("];

// Entities that are infrastructure, never exposed through GraphQL (G4).
const INTERNAL_ONLY = new Map([
  ["hive_schema_migrations", "migration ledger"],
  ["hive_schema_migration_lock", "migration lock"],
]);

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? walk(path) : path.endsWith(".rs") ? [path] : [];
  });
}

function code(path) {
  // Comments and doc comments do not count; string literals do.
  return readFileSync(path, "utf8").split("\n").map((line) => line.replace(/^\s*\/\/.*$/, ""));
}

function hits(files, tokens) {
  const found = [];
  for (const file of files) {
    code(file).forEach((line, index) => {
      for (const token of tokens) if (line.includes(token)) found.push({ file: relative(root, file), line: index + 1, token });
    });
  }
  return found;
}

const register = readFileSync(join(root, "docs/seaography-exceptions.md"), "utf8");
const granted = (hit) => register.includes(`${hit.file}:${hit.line}`) || register.includes(`\`${hit.file}\` ${hit.token}`);

const crates = join(root, "crates");
const sources = readdirSync(crates).flatMap((crate) =>
  ["src", "tests"].map((dir) => join(crates, crate, dir)).filter(existsSync).flatMap(walk));
const persistence = join(crates, "hive-persistence/src");
const violations = [];
const report = [];

// G1, G2
const g1 = hits(sources, RAW_SQL).filter((hit) => !granted(hit));
const g2 = hits(sources, ["Func::cust"]).filter((hit) => !granted(hit));
const byModule = new Map();
for (const hit of [...g1, ...g2]) {
  const match = hit.file.match(/^crates\/hive-persistence\/src\/([^/.]+)/);
  const key = match ? match[1] : hit.file.split("/").slice(0, 2).join("/");
  byModule.set(key, (byModule.get(key) ?? 0) + 1);
}
report.push(`G1 raw SQL: ${g1.length}   G2 Func::cust: ${g2.length}`);
for (const [key, count] of [...byModule].sort((a, b) => b[1] - a[1])) report.push(`   ${String(count).padStart(4)}  ${key}`);

if (onlyModule) {
  const count = byModule.get(onlyModule) ?? 0;
  console.log(`idiomatic gate, module ${onlyModule}: ${count} raw-SQL occurrence(s)`);
  for (const hit of [...g1, ...g2].filter((hit) => hit.file.includes(`/src/${onlyModule}`))) console.log(`  ${hit.file}:${hit.line} ${hit.token}`);
  process.exit(count === 0 ? 0 : 1);
}
if (g1.length) violations.push(`G1: ${g1.length} raw SQL occurrence(s)`);
if (g2.length) violations.push(`G2: ${g2.length} unlisted Func::cust`);

// G3
const g3 = hits(walk(join(crates, "hive-api/src")), HAND_BUILT).filter((hit) => !granted(hit));
report.push(`G3 hand-built GraphQL in hive-api: ${g3.length}`);
if (g3.length) violations.push(`G3: ${g3.length} hand-built GraphQL construct(s)`);

// G4, G5
const entityDir = join(persistence, "entity");
const entities = readdirSync(entityDir).filter((name) => name.endsWith(".rs") && name !== "mod.rs" && name !== "prelude.rs").map((name) => name.slice(0, -3));
const apiSource = walk(join(crates, "hive-api/src")).map((file) => code(file).join("\n")).join("\n");
const registered = new Set([...apiSource.matchAll(/register_entit(?:y|ies|y_modules)!\s*\(([\s\S]*?)\)\s*;/g)]
  .flatMap((match) => [...match[1].matchAll(/\b([a-z][a-z0-9_]*)\b/g)].map((word) => word[1])));
const unregistered = entities.filter((name) => !INTERNAL_ONLY.has(name) && !registered.has(name));
report.push(`G4 entities: ${entities.length}, not registered with Seaography: ${unregistered.length}`);
if (unregistered.length) violations.push(`G4: ${unregistered.length} entity module(s) neither registered nor internal-only`);

const known = new Set(entities);
const singular = (table) => table.replace(/ies$/, "y").replace(/s$/, "");
const lonely = entities.filter((name) => {
  const text = code(join(entityDir, `${name}.rs`)).join("\n");
  if (!/pub enum Relation \{\s*\}/.test(text)) return false;
  const columns = [...text.matchAll(/pub (\w+)_id: /g)].map((match) => match[1]);
  return columns.some((column) => [...known].some((entity) => entity !== name && singular(entity) === column));
});
report.push(`G5 entities with a foreign-key-like column and no Relation: ${lonely.length}`);
if (lonely.length) violations.push(`G5: ${lonely.length} entity module(s) missing relations`);

// G6
const sqlRs = existsSync(join(persistence, "sql.rs"));
const manifests = [join(root, "Cargo.toml"), ...readdirSync(crates).map((crate) => join(crates, crate, "Cargo.toml"))].filter(existsSync);
const sqlxManifests = manifests.filter((file) => /^\s*sqlx\b/m.test(readFileSync(file, "utf8"))).map((file) => relative(root, file));
report.push(`G6 sql.rs present: ${sqlRs}   manifests naming sqlx: ${sqlxManifests.length}`);
if (sqlRs) violations.push("G6: crates/hive-persistence/src/sql.rs still exists");
if (sqlxManifests.length) violations.push(`G6: sqlx in ${sqlxManifests.join(", ")}`);

// G7: a console query root may select only generated fields (camelCase entity module names).
const generatedRoots = new Set(entities.map((name) => name.replace(/_(\w)/g, (_, letter) => letter.toUpperCase())));
const consoleApi = join(crates, "hive-console/src/api");
const customRoots = [];
if (existsSync(consoleApi)) {
  for (const file of walk(consoleApi)) {
    const text = readFileSync(file, "utf8");
    for (const block of text.matchAll(/graphql_type = "Query"[^\n]*\n(?:[^\n]*\n)*?pub struct \w+ \{([\s\S]*?)\n\}/g)) {
      for (const field of block[1].matchAll(/pub (\w+):/g)) {
        const wire = field[1].replace(/_(\w)/g, (_, letter) => letter.toUpperCase());
        if (!generatedRoots.has(wire)) customRoots.push(`${relative(root, file)}: ${wire}`);
      }
    }
  }
}
report.push(`G7 console query roots that are not generated entity fields: ${customRoots.length}`);
if (customRoots.length) violations.push(`G7: ${customRoots.length} console query root(s) not generated`);

console.log(report.join("\n"));
if (violations.length) {
  console.log(`\n${enforce ? "FAIL" : "report mode"}: ${violations.length} gate(s) not met`);
  for (const violation of violations) console.log(`  ${violation}`);
  process.exit(enforce ? 1 : 0);
}
console.log("\nidiomatic gate: all of G1-G7 hold.");
