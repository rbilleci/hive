import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

const journeys = ["check:integration:mvp-shared", "check:e2e:agent-authoring", "check:e2e:deployment", "check:e2e:approval", "check:e2e:evaluation"];
for (const journey of journeys) {
  const result = spawnSync("npm", ["run", journey], { stdio: "inherit", env: process.env });
  assert.equal(result.status, 0, `The local MVP journey failed at ${journey}.`);
}
console.log("M17 local MVP journey evidence covers authoring, deployment, approval, and evaluation ownership routes.");
