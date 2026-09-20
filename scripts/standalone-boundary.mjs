import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";

// This repository builds, serves, and validates with no peer checkout. docs/ and evidence/ record
// the Java origin as history; nothing that compiles, runs, or gates may reach outside the tree.
const historical = /^(docs|evidence)\//;
const peerReference = /\.\.\/hive\b|\/projects\/hive(?![\w-])|\bhive\/(web|scripts|service|infra)\//;
const javaToolchain = /\bmvnw\b|\bpom\.xml\b|quarkus-run\.jar|\bJAVA_25_HOME\b/;

const tracked = execFileSync("git", ["ls-files", "--cached", "--others", "--exclude-standard"], { encoding: "utf8" })
  .split("\n").filter((path) => path && !historical.test(path) && path !== "scripts/standalone-boundary.mjs" && existsSync(path));
const findings = [];
for (const path of tracked) {
  if (/\.(png|ico|woff2?|lock)$|package-lock\.json$|\.terraform\.lock\.hcl$/.test(path)) continue;
  const lines = readFileSync(path, "utf8").split("\n");
  lines.forEach((line, index) => {
    if (peerReference.test(line)) findings.push(`${path}:${index + 1}: peer checkout reference: ${line.trim().slice(0, 120)}`);
    if (/\.(mjs|json|toml|ya?ml|tf)$|Dockerfile$/.test(path) && javaToolchain.test(line)) findings.push(`${path}:${index + 1}: Java toolchain reference: ${line.trim().slice(0, 120)}`);
  });
}
for (const required of ["crates/hive-console/Cargo.toml", "crates/hive-console/index.html", "schema/console-operations", "schema/contract.graphql", "schema/hive.graphql", "db/migration", "db/seed", "infra/local/compose.yaml", "Dockerfile"]) {
  if (!existsSync(required)) findings.push(`${required}: required by a standalone checkout and missing`);
}
assert.match(readFileSync("crates/hive-api/src/lib.rs", "utf8"), /env_string\("HIVE_WEB_DIST", "crates\/hive-console\/dist"\)/, "HIVE_WEB_DIST must default to the in-tree console build.");
assert.match(readFileSync("crates/hive-console/build.rs", "utf8"), /schema\/hive\.graphql/, "The console must check its operations against the in-tree schema.");

if (findings.length) {
  console.error(findings.join("\n"));
  console.error(`standalone-boundary: ${findings.length} finding(s).`);
  process.exit(1);
}
console.log(`standalone-boundary: ${tracked.length} files reach nothing outside this repository.`);
