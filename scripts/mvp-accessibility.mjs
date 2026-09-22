import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

const result = spawnSync("npm", ["run", "check:e2e:audit"], { stdio: "inherit", env: process.env });
assert.equal(result.status, 0, "The local audit accessibility journey failed.");
console.log("M17 local accessibility evidence covers the audit route and detail drawer.");
