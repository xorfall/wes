import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const binary = path.resolve(process.argv[2] ?? "target/release/wes");
const run = (bin, args) => execFileSync(bin, args, { encoding: "utf8", timeout: 60000 });
const inspected = JSON.parse(run("docker", ["inspect", "wes-postgres-demo"]))[0];
assert.equal(inspected.Config.Labels["com.docker.compose.project"], "wes-postgres-demo");
assert.equal(inspected.HostConfig.NetworkMode, "none");
assert.equal(Object.keys(inspected.HostConfig.PortBindings ?? {}).length, 0);
assert.equal(inspected.State.Health.Status, "healthy");
const psql = (...args) => run("docker", ["exec", "wes-postgres-demo",
  "/usr/lib/postgresql/17/bin/psql", "-XAt", "-U", "wes_reader", "-d", "wes_demo", ...args]).trim();
assert.equal(psql("-c", "SELECT current_user, current_database(), count(*) FROM public.products;"), "wes_reader|wes_demo|5");
assert.equal(psql("-c", "SHOW listen_addresses;"), "");
assert.equal(psql("-c", "SELECT has_table_privilege(current_user, 'public.products', 'SELECT'), has_table_privilege(current_user, 'public.products', 'INSERT'), has_table_privilege(current_user, 'public.products', 'UPDATE'), has_table_privilege(current_user, 'public.products', 'DELETE');"), "t|f|f|f");

const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "wes-postgres-check-"));
try {
  const output = run(binary, ["--home", path.join(temporary, "home"),
    "--env-file", path.join(here, "environments.yaml"), "--env", "pg-demo",
    "--file", path.join(here, "products.wes")]);
  const values = output.split("\n").filter(line => /^id[^:]*: /.test(line))
    .map(line => JSON.parse(line.slice(line.indexOf(": ") + 2)));
  const processResult = values.find(value => value && !Array.isArray(value) && "exitCode" in value);
  assert.equal(processResult?.exitCode, 0, output);
  assert.equal(processResult.stderr, "", output);
  const rows = values.find(Array.isArray);
  assert.equal(rows?.length, 5, output);
  assert.deepEqual(rows.map(row => row.id), [1, 2, 3, 4, 5]);
  assert.deepEqual(rows[0], { id: 1, name: "Keyboard", category: "Accessories", price: 49.9, stock: 25, active: true });
  assert.equal(rows[3].active, false);
  assert.equal(rows[3].stock, 0);
  console.log("PASS: actual environment + products.wes, 5 typed PostgreSQL rows, read-only role, no TCP/network/ports");
} finally {
  fs.rmSync(temporary, { recursive: true, force: true }); // Only this test's generated workspace.
}
