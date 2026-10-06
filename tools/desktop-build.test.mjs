import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {buildDesktop} from "./desktop-build.mjs";
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "wes-desktop-artifacts-"));
  t.after(() => fs.rmSync(root,{recursive:true,force:true}));
  const repository = path.join(root,"checkout"); fs.mkdirSync(repository);
  const shared = path.join(root,"shared-target"); fs.mkdirSync(shared);
  return {root:repository, shared};
}
function record(name, executable) { return {reason:"compiler-artifact", target:{name,kind:["bin"]},executable}; }
test("Windows bundles both actual Cargo artifacts from a shared target before building the GUI", t => {
  const {root, shared} = fixture(t); const validator = path.join(shared,"wes-view-build.exe"); const cli = path.join(shared,"wes.exe");
  fs.writeFileSync(validator,"synthetic validator"); fs.writeFileSync(cli,"synthetic console");
  const calls=[];
  buildDesktop({root, platform:"win32",env:{CARGO_TARGET_DIR:shared}, run:(command,args,options)=>{
    calls.push({command,args,options});
    if (command === "cargo") return [record("wes",cli),record("wes-view-build",validator)].map(JSON.stringify).join("\n");
    assert.equal(fs.readFileSync(path.join(root,"crates/desktop/resources/wes-terminal.exe"),"utf8"),"synthetic console");
    assert.equal(fs.readFileSync(path.join(root,"crates/desktop/resources/wes-view-build.exe"),"utf8"),"synthetic validator");
  }});
  assert.equal(calls.length,2); assert.equal(calls[1].command,"npm.cmd"); assert.equal(calls[0].options.env.CARGO_TARGET_DIR,shared);
  assert.ok(calls[0].args.includes("wes-views")); assert.ok(calls[0].args.includes("wes"));
});
test("the selected target controls native artifacts without substituting a host executable", t=>{
  const {root,shared} = fixture(t); const validator=path.join(shared,"wes-view-build"); fs.writeFileSync(validator,"wrong host program");
  let gui=false;
  assert.throws(()=>buildDesktop({root,platform:"darwin",env:{TAURI_ENV_TARGET_TRIPLE:"aarch64-pc-windows-msvc"},run:(command,args)=>{
    if (command !== "cargo") { gui=true; return; }
    assert.equal(args[args.indexOf("--target")+1],"aarch64-pc-windows-msvc");
    return JSON.stringify(record("wes-view-build",validator));
  }}),/matching native/);
  assert.equal(gui,false); assert.equal(fs.existsSync(path.join(root,"crates/desktop/resources")),false);
});
test("a native Unix package contains the validator and does not require a Windows console program",t=>{
  const {root,shared}=fixture(t); const validator=path.join(shared,"wes-view-build");fs.writeFileSync(validator,"synthetic native validator");
  buildDesktop({root,platform:"darwin",env:{},run:(command,args)=>{
    if(command === "cargo") { assert.ok(!args.includes("wes")); return JSON.stringify(record("wes-view-build",validator)); }
    assert.equal(command,"npm"); assert.equal(fs.existsSync(path.join(root,"crates/desktop/resources/wes-view-build")),true);
  }});
});
