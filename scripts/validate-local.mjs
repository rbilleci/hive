import { execFileSync, spawnSync } from "node:child_process";

// The full local gate. It validates one committed tree, so a pass names an exact candidate.
const candidate = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
const tree = execFileSync("git", ["rev-parse", "HEAD^{tree}"], { encoding: "utf8" }).trim();
const state = execFileSync("git", ["status", "--porcelain"], { encoding: "utf8" });
if (state) throw new Error("validate:local requires a clean committed candidate tree.");
console.log(`validate:local candidate=${candidate} tree=${tree} state=clean`);

const build = spawnSync("npm", ["run", "build"], { stdio: "inherit", env: process.env });
if (build.status !== 0) process.exit(build.status ?? 1);
// Every fixture launches this exact binary instead of rebuilding per check.
const environment = { ...process.env, HIVE_BINARY: process.env.HIVE_BINARY ?? `${process.cwd()}/target/release/hive` };

const packageScripts = JSON.parse(execFileSync("npm", ["pkg", "get", "scripts"], { encoding: "utf8" }));
const checks = [
  "check:standalone", "check:dsql-conformance", "check:rust", "check:architecture", "check:rust:database", "check:schema:entity-coverage", "check:schema:entity-relations", "check:schema:contract", "check:console", "check:console:operations",
  ...Object.keys(packageScripts).filter((name) => name.startsWith("check:integration:")),
  "check:packaging",
  ...Object.keys(packageScripts).filter((name) => name.startsWith("check:e2e:")),
  "check:mvp-acceptance", "check:mvp-accessibility"
];
for (const check of checks) {
  console.log(`validate:local running ${check}`);
  const result = spawnSync("npm", ["run", check], { stdio: "inherit", env: environment });
  if (result.status !== 0) {
    console.error(`validate:local failed at ${check}`);
    process.exit(result.status ?? 1);
  }
}
const finalCandidate = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
const finalTree = execFileSync("git", ["rev-parse", "HEAD^{tree}"], { encoding: "utf8" }).trim();
const finalState = execFileSync("git", ["status", "--porcelain"], { encoding: "utf8" });
if (finalCandidate !== candidate || finalTree !== tree || finalState) throw new Error("validate:local candidate, tree, or clean state changed during validation.");
console.log(`validate:local candidate=${finalCandidate} tree=${finalTree} state=clean checks=${checks.length} verified-after-checks`);
