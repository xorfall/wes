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

/*
 * Independent synthetic Dataset input package: a static View whose input record holds a read-only
 * Dataset<Int>. It reads pages only through context.datasets, by a pointer into its own input; it has
 * no Dataset outputs, state or events, no URL and no network capability.
 */
const DATASET_VIEW=`import {useEffect,useState} from "react";
import {defineView,numericText,ViewDatasetError,type DatasetPage,type NumericValue} from "@wes/view-sdk";
import {definition} from "./contract";

export default defineView(definition, {
  Component: ({input, context}) => {
    const reads = context.datasets;
    const [page, setPage] = useState<DatasetPage<NumericValue>>();
    const [problem, setProblem] = useState<string>();
    useEffect(() => {
      if (!reads) { setProblem("unavailable"); return; }
      let live = true;
      reads.page<NumericValue>("/samples", {from: "0"}, 20).then(
        next => { if (live) setPage(next); },
        (error: unknown) => { if (live) setProblem(error instanceof ViewDatasetError ? error.code : "failed"); });
      return () => { live = false; };
    }, [reads, input.samples.reference.generation]);
    return <section className="sample-page">
      <h3 className="screen-title">{input.title}</h3>
      <p className="screen-label">{input.samples.reference.records} committed samples</p>
      {problem ? <p className="screen-label">{problem}</p> : <ol>{page?.rows.map(row =>
        <li key={row.ordinal}>{row.ordinal}: <span className="table-value">{numericText(row.value)}</span></li>)}</ol>}
    </section>;
  },
});
`;
function datasetPackage(t) {
  const root=fs.mkdtempSync(path.join(os.tmpdir(),"wes-dataset-author-"));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  const source=path.join(root,"source");fs.cpSync(fixture,source,{recursive:true});
  const view=JSON.parse(fs.readFileSync(path.join(source,"view.json"),"utf8"));
  fs.writeFileSync(path.join(source,"view.json"),JSON.stringify({...view,name:"SamplePage",id:"sample-page",summary:"Read committed samples one bounded page at a time.",
    input:"SamplePage",outputs:{},interaction:undefined},null,2));
  fs.writeFileSync(path.join(source,"types.yaml"),"types:\n  SamplePage:\n    base: Record\n    fields:\n      title: Text\n      samples: Dataset<Int>\n");
  fs.writeFileSync(path.join(source,"View.tsx"),DATASET_VIEW);
  fs.writeFileSync(path.join(source,"view.css"),".sample-page{min-width:0}\n");
  return {source,output:path.join(root,"sample-page.wes-view.json")};
}

test("a read-only Dataset input package generates its descriptor type and builds natively",async t=>{
  const {source,output}=datasetPackage(t);
  const result=await build(source,output);
  assert.equal(result.ok,true);
  assert.equal(result.definition.name,"SamplePage");
  const definition=result.definition;
  const field=definition.contracts[definition.input].fields.samples;
  assert.deepEqual({kind:definition.contracts[field.type].kind,element:definition.contracts[field.type].element},{kind:"dataset",element:"Int"});
  assert.deepEqual(definition.outputs,{});
  assert.equal(definition.interaction,null);
  assert.ok(!Object.values(definition.contracts).some(schema=>schema.kind==="dataset"&&schema!==definition.contracts[field.type]));
  const generated=fs.readFileSync(path.join(source,"contract.ts"),"utf8");
  assert.match(generated,/import type \{ DatasetRef \} from "@wes\/view-sdk";/);
  assert.match(generated,/= DatasetRef<T\d+>;/);
  const artifact=JSON.parse(fs.readFileSync(output,"utf8"));
  assert.equal(artifact.definition,result.definition.digest);
  // Pages are requested through the frame's own port; the View holds no address of its own.
  assert.match(artifact.javascript,/dataset-read/);
  assert.doesNotMatch(artifact.javascript,/\/view-datasets\/|\/datasets\//);
});

test("a Dataset may not leave a View through interaction state, and the page API is typed",async t=>{
  const {source,output}=datasetPackage(t);
  const view=JSON.parse(fs.readFileSync(path.join(source,"view.json"),"utf8"));
  // Interaction state that reuses the Dataset-bearing input contract is refused natively.
  fs.writeFileSync(path.join(source,"view.json"),JSON.stringify({...view,interaction:{protocol:"SampleHold",state:"SamplePage",event:"Pick",sharedFields:[]}},null,2));
  fs.appendFileSync(path.join(source,"types.yaml"),"  Pick:\n    base: Record\n    fields:\n      ordinal: Text\n");
  await assert.rejects(build(source,output),error=>error.diagnostics?.some(d=>/read-only input port/.test(d.message)));
  assert.equal(fs.existsSync(output),false);
  const fresh=datasetPackage(t);
  fs.writeFileSync(path.join(fresh.source,"View.tsx"),DATASET_VIEW.replace('reads.page<NumericValue>("/samples"','reads.page<NumericValue>(42'));
  await assert.rejects(build(fresh.source,fresh.output),error=>error.diagnostics?.some(d=>d.file==="View.tsx"&&d.code.startsWith("TS")));
});

/*
 * The CI investigation's CiRecords View (examples/ci-investigation/record-view), built from an
 * isolated temporary copy so no generated contract.ts lands in the example directory. Its row
 * contract is the analysis's own CiLogLine closure, CiDigest's pattern included: a Dataset input is
 * accepted only when the committed schema digest equals this element's digest.
 */
const ciRecords=fileURLToPath(new URL("../../examples/ci-investigation/record-view/",import.meta.url));
const CI_DIGEST_PATTERN="^[0-9a-f]{64}$";
function ciRecordsCopy(t) {
  const root=fs.mkdtempSync(path.join(os.tmpdir(),"wes-ci-records-"));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  const source=path.join(root,"source");fs.cpSync(ciRecords,source,{recursive:true});
  fs.rmSync(path.join(source,"contract.ts"),{force:true});
  return {source,output:path.join(root,"ci-records.wes-view.json")};
}

test("CiRecords builds natively with the analysis's exact row contract, its pattern preserved",async t=>{
  const {source,output}=ciRecordsCopy(t);
  const result=await build(source,output);
  assert.equal(result.ok,true);
  const definition=result.definition;
  assert.equal(definition.name,"CiRecords");
  assert.deepEqual(definition.outputs,{});
  assert.equal(definition.interaction,null);
  const fields=definition.contracts[definition.input].fields;
  assert.deepEqual(Object.keys(fields).sort(),["outputs","receipt","state"]);
  const outputs=definition.contracts[fields.outputs.type];
  assert.deepEqual({kind:outputs.kind,element:outputs.element},{kind:"dataset",element:"CiLogLine"});
  assert.equal(Object.values(definition.contracts).filter(schema=>schema.kind==="dataset").length,1);
  const line=definition.contracts.CiLogLine;
  assert.deepEqual(Object.keys(line.fields).sort(),["artifact","ordinal","job","jobName","step","stepName","time","raw","text","level","group","byteStart","byteEnd","delimiterEnd"].sort());
  for(const span of ["byteStart","byteEnd","delimiterEnd"])assert.deepEqual(line.fields[span],{type:"Int",optional:false});
  assert.equal(definition.contracts.LogArtifactKey.fields.digest.type,"CiDigest");
  // The native identity of the row contract keeps its pattern; the browser host never evaluates it.
  assert.deepEqual(definition.contracts.CiDigest.constraints.patterns,[CI_DIGEST_PATTERN]);
  assert.deepEqual({min:definition.contracts.CiOrdinal.constraints.min,minLength:definition.contracts.CiId.constraints.minLength,maxLength:definition.contracts.CiId.constraints.maxLength},{min:"1",minLength:1,maxLength:1024});
  const artifact=JSON.parse(fs.readFileSync(output,"utf8"));
  assert.equal(artifact.definition,definition.digest);
  assert.match(artifact.javascript,/dataset-read/);
  assert.match(artifact.javascript,/\/outputs/);
  assert.doesNotMatch(artifact.javascript,/\/view-datasets\/|\/datasets\//);
  assert.ok(!fs.existsSync(path.join(ciRecords,"contract.ts")));
});

test("CiRecords generates its exact contract types and keeps the pattern in the generated definition",async t=>{
  const {source,output}=ciRecordsCopy(t);
  assert.equal((await build(source,output)).ok,true);
  const generated=fs.readFileSync(path.join(source,"contract.ts"),"utf8");
  assert.match(generated,/import type \{ DatasetRef \} from "@wes\/view-sdk";/);
  assert.match(generated,/= DatasetRef<T\d+>;/);
  assert.ok(generated.includes(JSON.stringify(CI_DIGEST_PATTERN)));
  assert.doesNotMatch(generated,/gui\/src|\.\.\//);
  assert.ok(!fs.existsSync(path.join(ciRecords,"contract.ts")));
});

test("CiRecords' row names do not admit a direct pattern input or a writable Dataset port",async t=>{
  const direct=ciRecordsCopy(t);
  const types=path.join(direct.source,"types.yaml");
  fs.writeFileSync(types,fs.readFileSync(types,"utf8").replace("fields: {state: CiLogState, outputs: 'Dataset<CiLogLine>', receipt: CiRecordsReceipt}",
    "fields: {state: CiLogState, outputs: 'Dataset<CiLogLine>', receipt: CiRecordsReceipt, digest: CiDigest}"));
  await assert.rejects(build(direct.source,direct.output),error=>error.diagnostics?.some(d=>d.code==="VIEW_CONTRACT"&&/Pattern-constrained view inputs/.test(d.message)));
  assert.equal(fs.existsSync(direct.output),false);
  const writable=ciRecordsCopy(t);
  const view=path.join(writable.source,"view.json");
  fs.writeFileSync(view,JSON.stringify({...JSON.parse(fs.readFileSync(view,"utf8")),interaction:{protocol:"CiRecordsHold",state:"CiRecords",event:"CiLogState",sharedFields:[]}},null,2));
  await assert.rejects(build(writable.source,writable.output),error=>error.diagnostics?.some(d=>d.code==="VIEW_CONTRACT"&&/read-only input port/.test(d.message)));
  assert.equal(fs.existsSync(writable.output),false);
});

/*
 * The shipped ExecutionComparison View, built from an isolated copy. Its standalone types repeat the
 * investigation recipe's InvestigationComparison exactly, so its input closure needs no other package.
 */
const executionComparison=fileURLToPath(new URL("../../views/execution-comparison/",import.meta.url));
const recipeTypes=fileURLToPath(new URL("../../examples/ci-investigation/investigation-types.yaml",import.meta.url));

test("ExecutionComparison builds standalone, read-only, with the recipe's exact comparison type",async t=>{
  const root=fs.mkdtempSync(path.join(os.tmpdir(),"wes-execution-comparison-"));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  const source=path.join(root,"source");fs.cpSync(executionComparison,source,{recursive:true});
  fs.rmSync(path.join(source,"contract.ts"),{force:true});
  const output=path.join(root,"execution-comparison.wes-view.json");
  const result=await build(source,output);
  assert.equal(result.ok,true,JSON.stringify(result));
  const definition=result.definition;
  assert.equal(definition.name,"ExecutionComparison");
  assert.equal(definition.id,"execution-comparison");
  assert.equal(definition.input,"ExecutionComparisonReport");
  assert.deepEqual(definition.outputs,{});
  assert.equal(definition.interaction,null);
  assert.deepEqual(Object.keys(definition.contracts[definition.input].fields).sort(),["comparisons","title"]);
  const comparison=definition.contracts.InvestigationComparison.fields;
  assert.deepEqual(Object.fromEntries(Object.entries(comparison).map(([name,field])=>[name,field.type])),{
    subject:"InvestigationKey",baseline:"InvestigationKey",target:"InvestigationKey",inputComparable:"Bool",environmentComparable:"Option<Bool>",
    targetExercised:"Bool",hypothesis:"Text",rationale:"Text",regressionRuledOut:"Bool"});
  assert.deepEqual({minLength:definition.contracts.InvestigationKey.constraints.minLength,maxLength:definition.contracts.InvestigationKey.constraints.maxLength},{minLength:1,maxLength:1024});
  // The package's declarations are the recipe's own, line for line.
  const recipe=fs.readFileSync(recipeTypes,"utf8");
  const own=fs.readFileSync(path.join(executionComparison,"types.yaml"),"utf8").split("\n");
  const key=own.find(line=>line.startsWith("  InvestigationKey:"));
  const fields=own[own.indexOf("  InvestigationComparison:")+2];
  for(const line of [key,fields])assert.ok(line&&recipe.split("\n").includes(line),line);
  // Read-only by declaration: no Dataset anywhere in its contracts, so the host grants it no page reads.
  assert.ok(!Object.values(definition.contracts).some(schema=>schema.kind==="dataset"));
  const artifact=JSON.parse(fs.readFileSync(output,"utf8"));
  assert.equal(artifact.definition,definition.digest);
  // The compiled bundle carries the shared SDK/frame code for every View, so its text is not checked.
  // What this package itself authors must not reach a read helper, the network or the host.
  const authored=fs.readFileSync(path.join(executionComparison,"View.tsx"),"utf8");
  const imports=[...authored.matchAll(/^import\s+(?:[^;]*?\sfrom\s+)?"([^"]+)"/gm)].map(match=>match[1]).sort();
  assert.deepEqual(imports,["./contract","./view.css","@wes/view-sdk"]);
  assert.match(authored,/^import \{defineView\} from "@wes\/view-sdk";$/m);
  assert.doesNotMatch(authored,/context\.datasets|\.page\(|fetch\(|XMLHttpRequest|WebSocket|\bwindow\.|import\(/);
  assert.doesNotMatch(fs.readFileSync(path.join(source,"contract.ts"),"utf8"),/gui\/src|\.\.\//);
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
