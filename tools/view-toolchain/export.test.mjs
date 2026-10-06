import {execFileSync} from "node:child_process";
import {test} from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {fileURLToPath} from "node:url";

test("the host exports a locked kit that builds without checkout imports or validator PATH access", t => {
  const repository = fileURLToPath(new URL("../../", import.meta.url));
  const suffix = process.platform === "win32" ? ".exe" : "";
  const host = process.env.WES_VIEW_HOST_BINARY || path.join(repository, "target/debug", `wes${suffix}`);
  assert.ok(fs.statSync(host).isFile(), "Build wes and wes-view-build before this check.");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "wes-export-kit-"));
  t.after(() => fs.rmSync(root, {recursive:true, force:true}));
  const home = path.join(root, "home"), kit = path.join(root, "kit");
  fs.mkdirSync(home);
  const env = {PATH:process.env.PATH, HOME:home, USERPROFILE:home,
    ...(process.env.SystemRoot ? {SystemRoot:process.env.SystemRoot} : {})};
  const run = (program, args, cwd=root, extra={}) => execFileSync(program, args,
    {cwd, env:{...env, ...extra}, encoding:"utf8", timeout:120000, stdio:"pipe"});
  const exported = JSON.parse(run(host, ["--export-view-toolchain", kit]));
  assert.equal(exported.dependenciesInstalled, false);
  assert.ok(fs.statSync(path.join(kit, "bin", `wes-view-build${suffix}`)).isFile());
  assert.throws(() => run(host, ["--export-view-toolchain", kit]), error => error.status === 1);
  const userConfig = path.join(root, "user.npmrc"), globalConfig = path.join(root, "global.npmrc");
  fs.writeFileSync(userConfig, ""); fs.writeFileSync(globalConfig, "");
  const npmOptions = ["--cache", path.join(root, "cache"), "--userconfig", userConfig,
    "--globalconfig", globalConfig, "--ignore-scripts", "--no-audit", "--no-fund",
    "--registry=https://registry.npmjs.org"];
  // Only explicit dependency preparation uses networking and an empty owned cache.
  run("npm", ["ci", ...npmOptions], kit);
  fs.rmSync(path.join(kit, "node_modules"), {recursive:true, force:true});
  run("npm", ["ci", ...npmOptions, "--offline", "--proxy=http://127.0.0.1:1",
    "--https-proxy=http://127.0.0.1:1"], kit);
  const wrapper = path.join(kit, "wes-view-package.mjs");
  const source = path.join(root, "source"), artifact = path.join(root, "view.wes-view.json");
  run(process.execPath, [wrapper, "init", source]);
  const built = JSON.parse(run(process.execPath, [wrapper, "build", source, artifact], kit));
  assert.equal(built.ok, true);
  const checked = JSON.parse(run(process.execPath, [wrapper, "check", artifact], kit));
  assert.equal(checked.ok, true);
  assert.ok(!fs.readFileSync(path.join(source, "contract.ts"), "utf8").includes(repository));
  assert.deepEqual(fs.readdirSync(home), []);
  const metadata = path.join(kit, "toolchain.json");
  const incompatible = JSON.parse(fs.readFileSync(metadata, "utf8"));
  incompatible.arch = "synthetic-other-architecture";
  fs.writeFileSync(metadata, JSON.stringify(incompatible));
  assert.throws(() => run(process.execPath, [wrapper, "describe"], kit), error => error.status === 1);
});
