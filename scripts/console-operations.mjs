import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parse } from "graphql";

// LFP-PARITY: the operation names in schema/console-operations are frozen. Audit rows, service logs,
// and the end-to-end checks identify a console request by its name, and cynic names an operation
// after its root struct. This check holds the two lists equal, so a renamed struct or a new
// operation without a reviewed document fails here.
const documented = new Map();
for (const file of readdirSync("schema/console-operations").filter((name) => name.endsWith(".graphql"))) {
  for (const definition of parse(readFileSync(join("schema/console-operations", file), "utf8")).definitions) {
    if (definition.kind === "OperationDefinition") documented.set(definition.name.value, definition.operation);
  }
}

const implemented = new Map();
const sources = join("crates", "hive-console", "src", "api");
for (const file of readdirSync(sources).filter((name) => name.endsWith(".rs"))) {
  const text = readFileSync(join(sources, file), "utf8");
  // A root struct written out: #[cynic(graphql_type = "Query", ...)] pub struct Name {
  for (const match of text.matchAll(/graphql_type = "(Query|Mutation)"[^\]]*\)\]\s*pub struct (\w+)/g)) implemented.set(match[2], match[1] === "Query" ? "query" : "mutation");
  // A root struct a macro writes: some_macro!($, Name, ...). The macro body's own `$root` is skipped above.
  for (const match of text.matchAll(/^(\w+)!\(\s*\$,\s*(\w+)/gm)) implemented.set(match[2], /mutation/.test(match[1]) ? "mutation" : "query");
}

const missing = [...documented.keys()].filter((name) => !implemented.has(name)).sort();
const undocumented = [...implemented.keys()].filter((name) => !documented.has(name)).sort();
const wrongKind = [...documented].filter(([name, kind]) => implemented.has(name) && implemented.get(name) !== kind).map(([name]) => name);
assert.deepEqual({ missing, undocumented, wrongKind }, { missing: [], undocumented: [], wrongKind: [] },
  "The console's cynic operations and schema/console-operations disagree.");
console.log(`console-operations: ${documented.size} operations match.`);
