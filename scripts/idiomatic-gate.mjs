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

// Entities that are infrastructure, never exposed through GraphQL (G4). Everything else in
// `entity/` is registered with Seaography. Each reason names why no GraphQL consumer reads the
// table: a ledger the service writes for itself, a lock, an outbox, a receipt, a write-side source
// of a view that *is* exposed, or an authorization grant whose effect is the computed
// `capabilities` field.
const INTERNAL_ONLY = new Map([
  ["hive_schema_migrations", "migration ledger"],
  ["hive_schema_migration_lock", "migration lock"],
  // Not a table: `entity/enums.rs` holds the shared `DeriveActiveEnum` definitions.
  ["enums", "not an entity: the shared active-enum definitions"],
  // The five per-domain audit tables the `audit_event_projection` view unions. The projection is
  // the exposed, redacted read surface (`sourceIp`/`userAgent` are `#[seaography(ignore)]` there
  // and gated on `AUDIT_SENSITIVE.VIEW`); the source tables carry those columns in the clear.
  ["administration_audit_events", "audit write-side source of audit_event_projection"],
  ["agent_authoring_audit_events", "audit write-side source of audit_event_projection"],
  ["agent_draft_audit_events", "audit write-side source of audit_event_projection"],
  ["configuration_audit_events", "audit write-side source of audit_event_projection"],
  ["deployment_audit_events", "audit write-side source of audit_event_projection"],
  // Write-side tables of the two projection views that are registered.
  ["agent_operational_summaries", "write-side table of agent_operational_view_projection"],
  ["project_dashboard_metrics", "write-side table of project_dashboard_projection"],
  // Outboxes and their delivery bookkeeping.
  ["deployment_outbox_events", "outbox"],
  ["evaluation_outbox_events", "outbox"],
  ["deployment_outbox_delivery_audit_repairs", "outbox delivery repair ledger"],
  // Worker heartbeats.
  ["deployment_worker_heartbeats", "worker heartbeat"],
  ["evaluation_worker_heartbeats", "worker heartbeat"],
  // Idempotency and replay receipts.
  ["deployment_approval_replay_receipts", "approval replay receipt"],
  ["deployment_recovery_action_receipts", "recovery command idempotency receipt"],
  ["evaluation_command_receipts", "evaluation command idempotency receipt"],
  // The approval scope caches and the archive boundary they are rebuilt against. No GraphQL path
  // reads them since the approval surface moved to the capability rule.
  ["deployment_approval_principal_organization_membership_scopes", "approval scope cache"],
  ["deployment_approval_principal_organization_scopes", "approval scope cache"],
  ["deployment_approval_principal_project_scopes", "approval scope cache"],
  ["deployment_approval_project_archive_events", "approval scope-cache archive boundary ledger"],
  // Single-purpose internal ledgers and allocators.
  ["deployment_approval_handoff_releases", "automatic approval handoff ledger"],
  ["deployment_project_quota_claims", "deployment quota claim ledger"],
  ["deployment_timeline_counters", "timeline sequence allocator"],
  // A stale internal projection: it reads only `project_memberships`, so it ignores the platform
  // role, organization roles and lifecycle status. Deliberately unqueried (plan, phase 6).
  ["effective_evaluation_capabilities", "internal capability projection, deliberately unqueried"],
  // Authorization grants. What a principal may do is the computed `capabilities` field on
  // `Organizations` / `Projects` / `Principals`, answered by the evaluator; the grant rows
  // themselves are the evaluator's input and are never on the wire.
  ["platform_role_assignments", "authorization grant read only by the capability evaluator"],
  ["console_role_assignments", "authorization grant read only by the capability evaluator"],
  // The legacy draft-editor grant: read-only since M-era Java (V007's own comment records that no
  // write path ever existed) and read by nothing in this codebase. `console_role_assignments` is
  // the evaluator's canonical source; `console-access.mjs` asserts that on purpose.
  ["agent_draft_editor_roles", "superseded draft-editor grant, read by nothing"],
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

// G3 counts *production* hand-built GraphQL. A `#[cfg(test)]` module is not production code: the
// remaining ones build throwaway `async_graphql::dynamic` schemas to exercise a scalar or an enum
// through the real engine, which is the only way to test those impls and is not a GraphQL tier.
// The blanking is exact — the `#[cfg(test)]` attribute, the `mod NAME {` it decorates, and the
// closing brace at that `mod`'s own indentation — so a token outside such a module still counts,
// and G1 (which must see `crates/*/tests`) keeps using `code()` unchanged.
function productionCode(path) {
  const lines = code(path);
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() !== "#[cfg(test)]") continue;
    const opening = (lines[index + 1] ?? "").match(/^( *)(?:pub(?:\([^)]*\))? )?mod \w+ \{$/);
    if (!opening) continue;
    const close = `${opening[1]}}`;
    let end = index + 2;
    while (end < lines.length && lines[end] !== close) end += 1;
    if (end === lines.length) continue; // unbalanced: count the module rather than lose it
    for (let blank = index; blank <= end; blank += 1) lines[blank] = "";
  }
  return lines;
}

// The one production `Object::new(` G3 does not count: Seaography's `Builder::new` bakes an
// internal `_ping` field onto `Mutation`, and replacing the base object before any mutation is
// registered is the only way to drop it (`schema/mod.rs` explains it at length). Matched on the
// exact file and the exact trimmed source line, so any other `Object::new(` — including another
// `Object::new("Mutation")` anywhere else — still counts.
const PING_WORKAROUND = {
  file: "crates/hive-api/src/schema/mod.rs",
  line: `builder.mutation = async_graphql::dynamic::Object::new("Mutation");`,
};

function pingWorkaround(hit, source) {
  return hit.file === PING_WORKAROUND.file && source.trim() === PING_WORKAROUND.line;
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

// G3: production hand-built GraphQL in `crates/hive-api/src`, excluding `#[cfg(test)]` modules
// and the single documented `_ping` line (both above). Nothing else is excluded.
const apiFiles = walk(join(crates, "hive-api/src"));
const g3 = [];
for (const file of apiFiles) {
  const lines = productionCode(file);
  lines.forEach((line, index) => {
    for (const token of HAND_BUILT) {
      if (!line.includes(token)) continue;
      const hit = { file: relative(root, file), line: index + 1, token };
      if (granted(hit) || pingWorkaround(hit, line)) continue;
      g3.push(hit);
    }
  });
}
report.push(`G3 hand-built GraphQL in hive-api: ${g3.length}`);
for (const hit of g3) report.push(`   ${hit.file}:${hit.line} ${hit.token}`);
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
//
// The fields are read off the struct the `#[cynic(graphql_type = "Query", ...)]` attribute
// actually decorates, not off "the next struct that looks like one". The previous regex scanned
// forward from the marker for `pub struct \w+ {`, which skipped over a macro *definition*'s
// `pub struct $root {` (`$root` is not `\w+`) and landed on an unrelated `QueryVariables` struct
// further down the file, counting its variables as query roots. Walking attribute -> item cannot
// do that: a struct is only ever read as a query root if it carries the attribute itself.
const generatedRoots = new Set(entities.map((name) => name.replace(/_(\w)/g, (_, letter) => letter.toUpperCase())));
const consoleApi = join(crates, "hive-console/src/api");
const customRoots = [];

function queryRootFields(text) {
  const lines = text.split("\n");
  const fields = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (!lines[index].includes("#[cynic(")) continue;
    // The attribute may span lines; take it up to the point its parentheses balance.
    let attribute = "";
    let depth = 0;
    let opened = false;
    let after = index;
    while (after < lines.length) {
      for (const character of lines[after]) {
        if (character === "(") { depth += 1; opened = true; } else if (character === ")") depth -= 1;
      }
      attribute += `${lines[after]}\n`;
      after += 1;
      if (opened && depth <= 0) break;
    }
    if (!/graphql_type\s*=\s*"Query"/.test(attribute)) continue;
    // The decorated item, past any further attributes, doc comments or blank lines.
    while (after < lines.length && /^\s*(#\[|\/\/|$)/.test(lines[after])) after += 1;
    const declaration = (lines[after] ?? "").match(/^( *)pub struct \w+ \{$/);
    if (!declaration) continue; // a macro body's `pub struct $root {`, which expands per call site
    const close = `${declaration[1]}}`;
    for (let body = after + 1; body < lines.length && lines[body] !== close; body += 1) {
      const field = lines[body].match(/^\s*pub (\w+):/);
      if (field) fields.push(field[1]);
    }
  }
  return fields;
}

if (existsSync(consoleApi)) {
  for (const file of walk(consoleApi)) {
    for (const field of queryRootFields(readFileSync(file, "utf8"))) {
      const wire = field.replace(/_(\w)/g, (_, letter) => letter.toUpperCase());
      if (!generatedRoots.has(wire)) customRoots.push(`${relative(root, file)}: ${wire}`);
    }
  }
}
report.push(`G7 console query roots that are not generated entity fields: ${customRoots.length}`);
for (const root of customRoots) report.push(`   ${root}`);
if (customRoots.length) violations.push(`G7: ${customRoots.length} console query root(s) not generated`);

console.log(report.join("\n"));
if (violations.length) {
  console.log(`\n${enforce ? "FAIL" : "report mode"}: ${violations.length} gate(s) not met`);
  for (const violation of violations) console.log(`  ${violation}`);
  process.exit(enforce ? 1 : 0);
}
console.log("\nidiomatic gate: all of G1-G7 hold.");
