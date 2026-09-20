import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const migrationRoot = "db/migration";
const queryRoot = "crates/hive-persistence/src";

const migrationFiles = readdirSync(migrationRoot)
    .filter((path) => path.endsWith(".sql"))
    .sort();
const queryFiles = readdirSync(queryRoot, { recursive: true })
    .filter((path) => path.endsWith(".rs"))
    .sort();

function findAll(source, pattern) {
  return [...source.matchAll(pattern)];
}

function lineOf(source, index) {
  return source.slice(0, index).split("\n").length;
}

// Every migration file's prose comments explain, in words, exactly the DDL forms this scanner looks
// for (e.g. "Aurora DSQL rejects CREATE RULE outright" or "no FOREIGN KEY"), since each removed
// constraint gets a comment recording why removing it was safe. Scanning raw source text would treat
// that explanation as another instance of the thing it explains — confirmed empirically: this scanner
// once misread its own prior "CREATE RULE outright" comment as a rule declaration named "outright".
// Blank out `-- ...` line-comment text (preserving every character's byte offset and every newline, so
// `lineOf` stays accurate) before matching, the same way a SQL lexer would. This codebase's migrations
// use only `--` line comments — confirmed no `/*` block comment exists under
// db/migration.
function stripLineComments(source) {
  return source.replace(/--[^\n]*/g, (match) => " ".repeat(match.length));
}

// The same reasoning as stripLineComments, applied to Java's comment styles: a persistence/query/*.java
// file's own explanatory comment about a removed pg_advisory_xact_lock call (e.g. documenting why an
// advisory lock is redundant under DSQL) contains the literal function-call text the scanner looks for.
// Confirmed empirically: PostgresEvaluationRepository.java tripped this exact false positive across a
// // line comment and two /** */ Javadoc blocks the Evaluation domain step added. Blank out both `//...`
// line comments and `/* ... */` block comments (preserving newlines, so line numbers stay accurate)
// before matching.
function stripJavaComments(source) {
  return source
      .replace(/\/\/[^\n]*/g, (match) => " ".repeat(match.length))
      .replace(/\/\*[\s\S]*?\*\//g, (match) => match.replace(/[^\n]/g, " "));
}

const findings = {
  references: [],
  triggers: [],
  functions: [],
  rules: [],
  sequences: [],
  advisoryLocks: []
};

for (const path of migrationFiles) {
  const source = stripLineComments(readFileSync(join(migrationRoot, path), "utf8"));

  for (const match of findAll(source, /\bREFERENCES\b/gi)) {
    findings.references.push({ path, line: lineOf(source, match.index) });
  }

  for (const match of findAll(source, /CREATE\s+TRIGGER\s+(\S+)/gi)) {
    findings.triggers.push({ path, line: lineOf(source, match.index), name: match[1] });
  }

  for (const match of findAll(source, /CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+([a-zA-Z_]\w*)\s*\(/gi)) {
    const tail = source.slice(match.index, match.index + 4000);
    const language = tail.match(/LANGUAGE\s+(plpgsql|sql)\b/i)?.[1]?.toLowerCase() ?? "unknown";
    const returnsTrigger = /RETURNS\s+trigger\b/i.test(tail.slice(0, tail.search(/LANGUAGE\s+(?:plpgsql|sql)\b/i) + 1 || tail.length));
    findings.functions.push({ path, line: lineOf(source, match.index), name: match[1], language, returnsTrigger });
  }

  for (const match of findAll(source, /\b(?:BIG|SMALL)?SERIAL\b|GENERATED\s+ALWAYS\s+AS\s+IDENTITY|CREATE\s+SEQUENCE/gi)) {
    findings.sequences.push({ path, line: lineOf(source, match.index), text: match[0] });
  }

  // Aurora DSQL rejects CREATE RULE outright (0A000 unsupported statement: Rule, confirmed against the
  // real cluster) -- a second, independent immutability mechanism from CREATE TRIGGER, used extensively
  // throughout this schema's "no_update"/"no_delete" DO INSTEAD NOTHING guards, predating the trigger-
  // based ones in several places and easy to miss since neither prior audit searched for it.
  for (const match of findAll(source, /CREATE\s+(?:OR\s+REPLACE\s+)?RULE\s+([a-zA-Z_]\w*)/gi)) {
    findings.rules.push({ path, line: lineOf(source, match.index), name: match[1] });
  }

  // Aurora DSQL rejects the entire pg_advisory_lock family outright (0A000 function ... not supported,
  // confirmed against the real cluster for pg_advisory_lock and pg_advisory_xact_lock) -- there is no
  // session- or transaction-scoped locking primitive under optimistic concurrency control. The pattern
  // covers every member of the family, not only the two confirmed above: the try_ non-blocking variants
  // (pg_try_advisory_lock, pg_try_advisory_xact_lock), the _shared variants of all four, and both
  // pg_advisory_unlock and pg_advisory_unlock_shared -- a real cluster rejects the whole pg_advisory_*
  // namespace identically, since none of it has meaning without session/transaction-scoped locking to
  // attach to. This regex's own widening from matching only pg_advisory_(xact_)?lock is itself a fix:
  // PostgresDeploymentRepository.java's expiredApprovalRequirementDeployments()/
  // compatibleApprovalHandoffDeployments() called pg_try_advisory_lock before the P-10 domain-closure
  // step removed both calls as redundant (transitionRequirement()'s own conditional UPDATE plus
  // SQLSTATE 40001 handling already makes the guarded operation safe without a lock) -- the narrower
  // pattern never matched either call site, leaving two real Aurora DSQL violations unreported by this
  // scanner for the whole time they existed; they were found only by inspection while tracing every
  // pg_advisory_lock call site by hand. Requiring the immediately-following "(" (rather than matching the bare
  // function name) is deliberate: it matches a real call site but not prose describing one, the same
  // reasoning stripLineComments exists for on the RULE scan above -- DatabaseMigrator.java's own
  // explanatory comment about replacing pg_advisory_lock names the function without calling it, and
  // would otherwise trip this exact scanner.
  for (const match of findAll(source, /\bpg_(?:try_)?advisory_(?:xact_)?(?:un)?lock(?:_(?:shared|all))?\s*\(/gi)) {
    findings.advisoryLocks.push({ path, line: lineOf(source, match.index), text: match[0].replace(/\s*\($/, "") });
  }
}

for (const path of queryFiles) {
  const source = stripJavaComments(readFileSync(join(queryRoot, path), "utf8"));
  for (const match of findAll(source, /\bpg_(?:try_)?advisory_(?:xact_)?(?:un)?lock(?:_(?:shared|all))?\s*\(/gi)) {
    findings.advisoryLocks.push({ path: "persistence/query/" + path, line: lineOf(source, match.index), text: match[0].replace(/\s*\($/, "") });
  }
}

const distinctTriggerNames = new Set(findings.triggers.map((f) => f.name));
const distinctFunctionNames = new Set(findings.functions.map((f) => f.name));
const plpgsqlFunctionNames = new Set(findings.functions.filter((f) => f.language === "plpgsql").map((f) => f.name));
const sqlFunctionNames = new Set(findings.functions.filter((f) => f.language === "sql").map((f) => f.name));
const triggerReturningFunctionNames = new Set(findings.functions.filter((f) => f.returnsTrigger).map((f) => f.name));

console.log("Aurora DSQL conformance scan — " + migrationFiles.length + " migration files, "
    + queryFiles.length + " persistence source files");
console.log("");
console.log("REFERENCES occurrences (foreign keys, column- and table-level): " + findings.references.length);
console.log("CREATE TRIGGER bindings: " + findings.triggers.length + " (" + distinctTriggerNames.size + " distinct trigger names)");
console.log("CREATE [OR REPLACE] FUNCTION declarations: " + findings.functions.length + " (" + distinctFunctionNames.size + " distinct names; "
    + plpgsqlFunctionNames.size + " distinct LANGUAGE plpgsql, " + sqlFunctionNames.size + " distinct LANGUAGE sql, "
    + triggerReturningFunctionNames.size + " distinct RETURNS trigger)");
console.log("CREATE RULE declarations: " + findings.rules.length + " (" + new Set(findings.rules.map((f) => f.name)).size + " distinct rule names)");
console.log("SERIAL / GENERATED ALWAYS AS IDENTITY / CREATE SEQUENCE occurrences: " + findings.sequences.length);
console.log("pg_advisory_lock family call sites (lock/try_lock/unlock, xact- and shared-scoped): " + findings.advisoryLocks.length);
console.log("");

function printGroup(title, items, formatter) {
  if (items.length === 0) return;
  console.log(title + ":");
  for (const item of items) console.log("  " + formatter(item));
  console.log("");
}

printGroup("REFERENCES sites", findings.references, (f) => `${f.path}:${f.line}`);
printGroup("Triggers", findings.triggers, (f) => `${f.path}:${f.line} — ${f.name}`);
printGroup("Functions", findings.functions, (f) => `${f.path}:${f.line} — ${f.name} (LANGUAGE ${f.language}${f.returnsTrigger ? ", RETURNS trigger" : ""})`);
printGroup("Rules", findings.rules, (f) => `${f.path}:${f.line} — ${f.name}`);
printGroup("Sequence-shaped identifiers", findings.sequences, (f) => `${f.path}:${f.line} — ${f.text}`);
printGroup("Advisory lock call sites", findings.advisoryLocks, (f) => `${f.path}:${f.line} — ${f.text}`);

const violationCount = findings.references.length + findings.triggers.length + findings.functions.length
    + findings.rules.length + findings.sequences.length + findings.advisoryLocks.length;

if (violationCount > 0) {
  console.log(violationCount + " Aurora DSQL conformance violation(s) found. This is the authoritative inventory for the DSQL rewrite plan; "
      + "it is not yet wired into validate:local's mandatory gate because the current schema is expected to fail it. "
      + "Wire it into validate-local.mjs once the schema and JPA entities are DSQL-clean.");
  process.exitCode = 1;
} else {
  console.log("No foreign keys, triggers, PL/pgSQL functions, rules, sequence-shaped identifiers, or pg_advisory_lock-family call sites found. Schema and persistence code are Aurora DSQL-conformant.");
}
