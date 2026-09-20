import assert from "node:assert/strict";
import {
  createIsolatedDatabase,
  reserveLocalPort,
  startIsolatedLocalService,
  startLocalDeploymentWorker,
  startLocalService
} from "./local-service.mjs";

let first;
let second;
let firstService;
let secondService;
let firstWorker;
let secondWorker;
let blockedPort;
let siblingService;
try {
  first = await createIsolatedDatabase("hive_m13_harness_a");
  second = await createIsolatedDatabase("hive_m13_harness_b");
  const services = await Promise.allSettled([
    startIsolatedLocalService(first.name), startIsolatedLocalService(second.name)
  ]);
  firstService = services[0].status === "fulfilled" ? services[0].value : undefined;
  secondService = services[1].status === "fulfilled" ? services[1].value : undefined;
  if (services.some((result) => result.status === "rejected")) throw services.find((result) => result.status === "rejected").reason;
  assert.notEqual(firstService.port, secondService.port);
  const workers = await Promise.allSettled([
    startLocalDeploymentWorker(first.name), startLocalDeploymentWorker(second.name)
  ]);
  firstWorker = workers[0].status === "fulfilled" ? workers[0].value : undefined;
  secondWorker = workers[1].status === "fulfilled" ? workers[1].value : undefined;
  if (workers.some((result) => result.status === "rejected")) throw workers.find((result) => result.status === "rejected").reason;
  blockedPort = await reserveLocalPort();
  const collision = await Promise.allSettled([
    startLocalService(blockedPort.port, first.name), startIsolatedLocalService(first.name)
  ]);
  siblingService = collision[1].status === "fulfilled" ? collision[1].value : undefined;
  assert.equal(collision[0].status, "rejected");
  if (collision[0].status === "rejected") assert.match(String(collision[0].reason?.message ?? collision[0].reason), /local service ended before health verification/i);
  assert.equal(collision[1].status, "fulfilled");
} finally {
  const stopped = await Promise.allSettled([
    blockedPort?.release(), firstWorker?.stop(), secondWorker?.stop(), siblingService?.stop(), firstService?.stop(), secondService?.stop()
  ].filter(Boolean));
  const dropped = await Promise.allSettled([first?.drop(), second?.drop()].filter(Boolean));
  const failures = [...stopped, ...dropped].filter((result) => result.status === "rejected");
  if (failures.length) throw new AggregateError(failures.map((result) => result.reason), "The deployment harness cleanup failed.");
}
