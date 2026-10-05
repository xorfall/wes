import {execFileSync} from "node:child_process";
import {test} from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {fileURLToPath} from "node:url";
import {build, init, describe} from "./index.mjs";

const fixture = fileURLToPath(new URL("./template/",import.meta.url));

test("native compilation without workspace dependencies gives a root npm ci instruction",t=>{
  const root=fs.mkdtempSync(path.join(os.tmpdir(),"wes-missing-dependencies-"));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  for(const file of ["native-build.mjs","index.mjs","contracts.mjs"]) {
    fs.copyFileSync(fileURLToPath(new URL(file,import.meta.url)),path.join(root,file));
  }
  try {
    execFileSync(process.execPath,[path.join(root,"native-build.mjs")],{encoding:"utf8",stdio:"pipe",env:{PATH:process.env.PATH}});
    assert.fail("expected a missing-dependency failure");
  } catch(error) {
    assert.equal(error.status,1);
    assert.match(error.stderr,/npm ci from the Wes repository root/);
    assert.match(error.stderr,/before cargo build/);
  }
});

test("agent discovery and scaffolding do not require source access or overwrite author files",t=>{
  const root=fs.mkdtempSync(path.join(os.tmpdir(),"wes-sdk-"));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  const source=path.join(root,"source");
  assert.equal(init(source).ok,true);
  assert.throws(()=>init(source),/never overwrites/);
  assert.match(describe().sourceFiles["View.tsx"],/defineView/);
  assert.match(describe().sourceFiles["view.css"],/Optional.*Import/);
  assert.deepEqual(describe().layout.tiers,["preview","expanded","window"]);
  assert.match(describe().layout.placement.semantics,/dashboard may override/);
  assert.match(describe().layout.allocation,/feedback loop/);
  const theme=JSON.parse(fs.readFileSync(fileURLToPath(new URL("../../packages/view-sdk/theme.json",import.meta.url)),"utf8"));
  assert.deepEqual(describe().theme,theme);
  assert.equal(theme.roles["table-value"].style["font-family"],"var(--type-mono-family)");
  assert.ok(!JSON.stringify(theme).includes("#"));
  assert.deepEqual(fs.readdirSync(source).sort(),["View.tsx","types.yaml","view.css","view.json"].sort());
});
function isolated(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(),"wes-author-"));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  const source = path.join(root,"source"); fs.cpSync(fixture,source,{recursive:true});
  return {source,output:path.join(root,"task-board.wes-view.json")};
}

test("an author package outside the repository builds without application source imports",async t=>{
  const {source,output}=isolated(t);
  const result=await build(source,output);
  assert.equal(result.ok,true);
  assert.equal(result.definition.name,"TaskBoard");
  assert.equal(result.digest.length,64);
  const artifact=JSON.parse(fs.readFileSync(output,"utf8"));
  assert.equal(artifact.definition,result.definition.digest);
  assert.match(artifact.css,/task-board/);
  assert.match(artifact.javascript,/TaskBoard/);
  assert.doesNotMatch(fs.readFileSync(path.join(source,"contract.ts"),"utf8"),/gui\/src|\.\.\//);
  assert.ok(!fs.readdirSync(path.dirname(output)).some(name=>name.endsWith(".tmp")));
});

test("TypeScript failures have author file/line locations and leave the previous artifact intact",async t=>{
  const {source,output}=isolated(t);
  await build(source,output); const previous=fs.readFileSync(output,"utf8");
  const view=path.join(source,"View.tsx");
  fs.writeFileSync(view,fs.readFileSync(view,"utf8").replace("input.title","input.missing"));
  await assert.rejects(build(source,output),error=>error.diagnostics.some(d=>d.file==="View.tsx"&&d.line>0&&d.message.includes("missing")));
  assert.equal(fs.readFileSync(output,"utf8"),previous);
});

test("contract errors and imports outside the package are refused before activation",async t=>{
  const {source,output}=isolated(t);
  const view=path.join(source,"View.tsx");
  fs.writeFileSync(path.join(source,"../outside.ts"),"export const outside = 1;");
  fs.appendFileSync(view,'\nimport {outside} from "../outside"; console.log(outside);\n');
  await assert.rejects(build(source,output));
  assert.equal(fs.existsSync(output),false);
  fs.writeFileSync(path.join(source,"types.yaml"),"types: {TaskBoard: {base: Missing}}");
  await assert.rejects(build(source,output),error=>error.diagnostics?.[0]?.code==="VIEW_CONTRACT");
});

test("installed bin symlinks execute the same machine-readable CLI",t=>{
 const root=fs.mkdtempSync(path.join(os.tmpdir(),"wes-view-bin-"));t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
 const bin=path.join(root,"wes-view-package");fs.symlinkSync(fileURLToPath(new URL("./index.mjs",import.meta.url)),bin);
 const result=JSON.parse(execFileSync(process.execPath,[bin,"describe"],{encoding:"utf8"}));assert.equal(result.sdk,describe().sdk);
});

test("packed SDK and compiler build through an installed CLI outside the monorepo",async t=>{
 const {source,output}=isolated(t),root=path.dirname(source),repo=fileURLToPath(new URL("../../",import.meta.url));
 for(const workspace of ["@wes/view-sdk","wes-view-package"])execFileSync("npm",["pack","--workspace",workspace,"--pack-destination",root],{cwd:repo,stdio:"pipe"});
 const sdk=path.join(root,"wes-view-sdk-1.0.0.tgz"),compiler=path.join(root,"wes-view-package-1.0.0.tgz"),installation=path.join(root,"installed"),cache=path.join(root,"npm-cache");
 fs.mkdirSync(installation);
 fs.writeFileSync(path.join(installation,"package.json"),JSON.stringify({name:"wes-view-package-fixture",version:"1.0.0",private:true,dependencies:{"@wes/view-sdk":`file:../${path.basename(sdk)}`,"wes-view-package":`file:../${path.basename(compiler)}`}}));
 const userConfig=path.join(root,"npm-user.rc"),globalConfig=path.join(root,"npm-global.rc");
 fs.writeFileSync(userConfig,"");fs.writeFileSync(globalConfig,"");
 // Dependency preparation is explicit and uses an empty fixture-owned cache.
 // npm ci in the checkout does not cache the registry metadata needed by a new install.
 const npmOptions=["--cache",cache,"--userconfig",userConfig,"--globalconfig",globalConfig,"--ignore-scripts","--no-audit","--no-fund"];
 execFileSync("npm",["install",...npmOptions,"--registry=https://registry.npmjs.org"],{cwd:installation,stdio:"pipe",timeout:120000});
 fs.rmSync(path.join(installation,"node_modules"),{recursive:true,force:true});
 // Reinstall from the lock and prepared cache with networking disabled. Keep
 // the registry identity (cache keys), but make network fallback unreachable.
 execFileSync("npm",["ci",...npmOptions,"--offline","--registry=https://registry.npmjs.org","--proxy=http://127.0.0.1:1","--https-proxy=http://127.0.0.1:1"],{cwd:installation,stdio:"pipe",timeout:30000});
 const result=JSON.parse(execFileSync(process.execPath,[path.join(installation,"node_modules/.bin/wes-view-package"),"build",source,output],{encoding:"utf8",timeout:30000}));
 assert.equal(result.ok,true,JSON.stringify(result));assert.equal(result.definition.name,"TaskBoard");
 const discovery=JSON.parse(execFileSync(process.execPath,[path.join(installation,"node_modules/.bin/wes-view-package"),"describe"],{encoding:"utf8"}));
 assert.deepEqual(discovery,describe());
 assert.match(JSON.parse(fs.readFileSync(output,"utf8")).javascript,/wes-view-theme/);
});
