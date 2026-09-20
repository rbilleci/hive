import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import {
  buildSchema, getNamedType, isEnumType, isInputObjectType, isInterfaceType, isObjectType, isUnionType, parse, visit
} from "graphql";

// RTD-SDL-CUSTOM-TIER: every type the console operation documents can reach must match the frozen
// contract in schema/contract.graphql member for member. The generated read tier (RTD-GENERATED-READ-TIER)
// is unreachable from the console documents and is therefore outside this comparison.
const binary = join(process.cwd(), "target", "release", "hive");
assert.ok(existsSync(binary), "Build the release binary first: cargo build --release");
const runtimeSdl = execFileSync(binary, ["schema-sdl"], { encoding: "utf8" });
assert.equal(readFileSync("schema/hive.graphql", "utf8"), runtimeSdl, "schema/hive.graphql is stale; run npm run generate:schema.");

const contract = buildSchema(readFileSync("schema/contract.graphql", "utf8"));
const runtime = buildSchema(runtimeSdl);
const differences = [];

function describeArguments(args) {
  return Object.fromEntries(args.map((argument) => [argument.name, { type: String(argument.type), defaultValue: argument.defaultValue }]));
}

function describeFields(type) {
  return Object.fromEntries(Object.values(type.getFields()).map((field) => [field.name, {
    type: String(field.type),
    deprecationReason: field.deprecationReason ?? null,
    ...(field.args ? { args: describeArguments(field.args) } : { defaultValue: field.defaultValue })
  }]));
}

function describe(type) {
  if (isEnumType(type)) return { kind: "enum", values: type.getValues().map((value) => value.name).sort() };
  if (isUnionType(type)) return { kind: "union", members: type.getTypes().map((member) => member.name).sort() };
  if (isObjectType(type) || isInterfaceType(type)) {
    return { kind: isObjectType(type) ? "object" : "interface", interfaces: type.getInterfaces().map((entry) => entry.name).sort(), fields: describeFields(type) };
  }
  if (isInputObjectType(type)) return { kind: "input", fields: describeFields(type) };
  return { kind: "scalar" };
}

function compareMembers(path, expected, actual) {
  for (const name of Object.keys(expected)) {
    if (!(name in actual)) { differences.push(`${path}.${name}: missing from the runtime schema`); continue; }
    try { assert.deepEqual(actual[name], expected[name]); }
    catch { differences.push(`${path}.${name}: contract ${JSON.stringify(expected[name])} runtime ${JSON.stringify(actual[name])}`); }
  }
  for (const name of Object.keys(actual)) if (!(name in expected)) differences.push(`${path}.${name}: not in the contract`);
}

/** Collects every named type reachable from the root fields the console documents select. */
function reachableTypes() {
  const documents = ["schema/console-operations"].flatMap((directory) => execFileSync("find", [directory, "-name", "*.graphql"], { encoding: "utf8" }).trim().split("\n"));
  const roots = { Query: new Set(), Mutation: new Set() };
  for (const file of documents) {
    visit(parse(readFileSync(file, "utf8")), {
      OperationDefinition(node) {
        const root = node.operation === "query" ? roots.Query : roots.Mutation;
        for (const selection of node.selectionSet.selections) if (selection.kind === "Field") root.add(selection.name.value);
      }
    });
  }
  const seen = new Set();
  const walk = (type) => {
    const named = getNamedType(type);
    if (seen.has(named.name) || named.name.startsWith("__")) return;
    seen.add(named.name);
    if (isUnionType(named)) named.getTypes().forEach(walk);
    if (isInterfaceType(named)) contract.getPossibleTypes(named).forEach(walk);
    if (isObjectType(named) || isInterfaceType(named)) {
      named.getInterfaces().forEach(walk);
      for (const field of Object.values(named.getFields())) { walk(field.type); field.args.forEach((argument) => walk(argument.type)); }
    }
    if (isInputObjectType(named)) Object.values(named.getFields()).forEach((field) => walk(field.type));
  };
  for (const [rootName, fieldNames] of Object.entries(roots)) {
    const contractRoot = contract.getType(rootName).getFields();
    const runtimeRoot = runtime.getType(rootName)?.getFields() ?? {};
    for (const fieldName of [...fieldNames].sort()) {
      if (fieldName === "__typename") continue;
      const field = contractRoot[fieldName];
      assert.ok(field, `The console selects ${rootName}.${fieldName}, which the contract does not declare.`);
      walk(field.type); field.args.forEach((argument) => walk(argument.type));
      const actual = runtimeRoot[fieldName];
      if (!actual) { differences.push(`${rootName}.${fieldName}: missing from the runtime schema`); continue; }
      const expected = { type: String(field.type), args: describeArguments(field.args) };
      const observed = { type: String(actual.type), args: describeArguments(actual.args) };
      try { assert.deepEqual(observed, expected); }
      catch { differences.push(`${rootName}.${fieldName}: contract ${JSON.stringify(expected)} runtime ${JSON.stringify(observed)}`); }
    }
  }
  return [...seen].sort();
}

/** Descriptions are part of the frozen contract: codegen projects them into the committed client. */
function compareDescriptions(name) {
  const expected = contract.getType(name), actual = runtime.getType(name);
  const note = (path, left, right) => { if ((left ?? null) !== (right ?? null)) differences.push(`${path} description: contract ${JSON.stringify(left ?? null)} runtime ${JSON.stringify(right ?? null)}`); };
  note(name, expected.description, actual.description);
  if (isEnumType(expected) && isEnumType(actual)) for (const value of expected.getValues()) note(`${name}.${value.name}`, value.description, actual.getValue(value.name)?.description);
  if (typeof expected.getFields !== "function" || typeof actual.getFields !== "function") return;
  for (const field of Object.values(expected.getFields())) {
    const counterpart = actual.getFields()[field.name];
    if (!counterpart) continue;
    note(`${name}.${field.name}`, field.description, counterpart.description);
    for (const argument of field.args ?? []) note(`${name}.${field.name}(${argument.name})`, argument.description, counterpart.args.find((entry) => entry.name === argument.name)?.description);
  }
}

const types = reachableTypes();
for (const name of [...types, "Query", "Mutation"]) if (runtime.getType(name)) compareDescriptions(name);
for (const name of types) {
  const expected = describe(contract.getType(name));
  const actualType = runtime.getType(name);
  if (!actualType) { differences.push(`${name}: missing from the runtime schema`); continue; }
  const actual = describe(actualType);
  if (expected.kind !== actual.kind) { differences.push(`${name}: contract kind ${expected.kind}, runtime kind ${actual.kind}`); continue; }
  if (expected.fields) compareMembers(name, expected.fields, actual.fields);
  for (const key of ["values", "members", "interfaces"]) {
    if (expected[key] && JSON.stringify(expected[key]) !== JSON.stringify(actual[key])) differences.push(`${name} ${key}: contract ${expected[key]} runtime ${actual[key]}`);
  }
}

if (differences.length) {
  console.error(differences.join("\n"));
  console.error(`schema-contract: ${differences.length} difference(s) across ${types.length} console-reachable types.`);
  process.exit(1);
}
console.log(`schema-contract: ${types.length} console-reachable types match the contract.`);
