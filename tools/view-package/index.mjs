#!/usr/bin/env node
/** One-shot independent view compiler. Source code is checked and bundled, never evaluated. */
import fs from "node:fs";
import path from "node:path";
import {fileURLToPath} from "node:url";
import {createRequire} from "node:module";
import {execFileSync} from "node:child_process";
import {randomUUID} from "node:crypto";
import {generateContract} from "./contracts.mjs";

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const validator = process.env.WES_VIEW_CONTRACT_TOOL || "wes-view-build";
const diagnostics = error => error?.diagnostics ?? error?.errors?.slice(0,32).map(issue=>({code:"VIEW_BUNDLE",message:issue.text,...(issue.location?{file:issue.location.file,line:issue.location.line,column:issue.location.column+1}:{})})) ?? [{code:"VIEW_BUILD",message:error?.message ?? String(error)}];

const authoring = JSON.parse(fs.readFileSync(require.resolve("@wes/view-sdk/authoring.json"),"utf8"));
const theme = JSON.parse(fs.readFileSync(require.resolve("@wes/view-sdk/theme.json"),"utf8"));
export function describe() { return {...authoring,theme}; }

export function init(directory) {
  const target=path.resolve(directory);
  if (fs.existsSync(target)) throw new Error("Choose a new source directory; init never overwrites files");
  fs.cpSync(path.join(here,"template"),target,{recursive:true,errorOnExist:true});
  return {ok:true,source:target,next:`wes-view-package build ${JSON.stringify(target)} OUTPUT.wes-view.json`};
}

function contracts(args) {
  try { return JSON.parse(execFileSync(validator, args, {encoding:"utf8",timeout:30_000,maxBuffer:9*1024*1024})); }
  catch (error) {
    let result; try { result = JSON.parse(String(error.stdout)); } catch {}
    const failure = new Error(result?.diagnostics?.[0]?.message ?? `Contract validator failed: ${error.message}`);
    failure.diagnostics = result?.diagnostics?.map(d=>{const position=d.message.match(/line[: ]+(\d+)[, ]+column[: ]+(\d+)/i);return {...d,...(position?{file:d.message.includes("TYP")?"types.yaml":"view.json",line:Number(position[1]),column:Number(position[2])}:{})};});
    throw failure;
  }
}

function typecheck(entry, source) {
  const ts = require("typescript");
  const options = {strict:true,noEmit:true,target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.ESNext,
    moduleResolution:ts.ModuleResolutionKind.Bundler,jsx:ts.JsxEmit.ReactJSX,skipLibCheck:true,
    esModuleInterop:true,resolveJsonModule:true,types:[],lib:["lib.es2020.d.ts","lib.dom.d.ts"]};
  const host = ts.createCompilerHost(options);
  host.resolveModuleNames = (names, containingFile) => names.map(name => {
    if (name === "@wes/view-sdk") return {resolvedFileName:require.resolve(name),extension:ts.Extension.Ts,isExternalLibraryImport:true};
    if (name === "@wes/view-sdk/theme") return {resolvedFileName:require.resolve(name),extension:ts.Extension.Ts,isExternalLibraryImport:true};
    if (name === "react" || name === "react/jsx-runtime") {
      return {resolvedFileName:path.join(path.dirname(require.resolve("@types/react/package.json")),name === "react" ? "index.d.ts" : "jsx-runtime.d.ts"),extension:ts.Extension.Dts,isExternalLibraryImport:true};
    }
    return ts.resolveModuleName(name,containingFile,options,host).resolvedModule ?? ts.resolveModuleName(name,path.join(here,"dependency.ts"),options,host).resolvedModule;
  });
  const program = ts.createProgram([entry],options,host);
  const problems = ts.getPreEmitDiagnostics(program);
  if (problems.length) {
    const error = new Error("View TypeScript validation failed");
    error.diagnostics = problems.slice(0,32).map(d => {
      const position = d.file && d.start !== undefined ? d.file.getLineAndCharacterOfPosition(d.start) : undefined;
      return {code:`TS${d.code}`,message:ts.flattenDiagnosticMessageText(d.messageText,"\n"),
        ...(d.file ? {file:path.relative(source,d.file.fileName)} : {}),
        ...(position ? {line:position.line+1,column:position.character+1} : {})};
    });
    throw error;
  }
}

/** Both the standalone compiler and the native build use this compilation pass.
 * Metadata must come from the native contract validator; this pass never evaluates author code. */
export async function compile(source, metadata) {
  const root = fs.realpathSync(source);
  fs.writeFileSync(path.join(root,"contract.ts"),generateContract(metadata.definition));
  const manifest = JSON.parse(metadata.manifest);
  const entry = path.join(root,manifest.renderer);
  typecheck(entry,root);
  const esbuild = require("esbuild");
  const sdk = fs.realpathSync(path.dirname(require.resolve("@wes/view-sdk")));
  const dependencies = path.join(here,"node_modules");
  const dependencyRoots = ["react", "react-dom", "scheduler", "lossless-json"].map(name => {
    let directory = path.dirname(fs.realpathSync(require.resolve(name)));
    while (!fs.existsSync(path.join(directory,"package.json")) || JSON.parse(fs.readFileSync(path.join(directory,"package.json"),"utf8")).name !== name) {
      const parent=path.dirname(directory);
      if (parent===directory) throw new Error(`Dependency package root not found: ${name}`);
      directory=parent;
    }
    return directory;
  });
  const within = (file, directory) => file === directory || file.startsWith(directory+path.sep);
  const result = await esbuild.build({stdin:{contents:`import view from ${JSON.stringify(entry)}; import {startFrame} from ${JSON.stringify(path.join(sdk,"frame.tsx"))}; startFrame(view);`,resolveDir:root,sourcefile:"wes-frame.tsx",loader:"tsx"},bundle:true,write:false,format:"iife",platform:"browser",
    define:{"process.env.NODE_ENV":JSON.stringify("production")},minify:true,target:"es2020",jsx:"automatic",outdir:"output",nodePaths:[dependencies],
    alias:{"@wes/view-sdk":path.join(sdk,"index.ts"),react:dependencyRoots[0],"react-dom":dependencyRoots[1],scheduler:dependencyRoots[2]},metafile:true,logLevel:"silent",
    plugins:[{name:"confined-source",setup(builder){builder.onLoad({filter:/.*/},args => {
      const file = fs.realpathSync(args.path);
      if (![root,sdk,dependencies,...dependencyRoots].some(dir=>within(file,dir))) return {errors:[{text:"View imports must stay in the source package or the supplied SDK/dependencies"}]};
      if (fs.statSync(file).size > 4*1024*1024) return {errors:[{text:"View source file exceeds 4 MiB"}]};
      return undefined;
    });}}]});
  if (Object.values(result.metafile.outputs).some(out=>out.imports.length)) throw new Error("Compiled views cannot contain unresolved runtime imports");
  const javascript = result.outputFiles.find(f=>f.path.endsWith(".js"))?.text;
  const css = result.outputFiles.find(f=>f.path.endsWith(".css"))?.text ?? "";
  return {format:1,sdk:authoring.sdk,manifest:metadata.manifest,types:metadata.types,definition:metadata.definition.digest,javascript,css};
}

export async function build(source, output) {
  const metadata = contracts(["--source",fs.realpathSync(source)]);
  const artifact = await compile(source,metadata);
  const target = path.resolve(output);
  fs.mkdirSync(path.dirname(target),{recursive:true});
  const temporary = `${target}.${randomUUID()}.tmp`;
  try {
    fs.writeFileSync(temporary,JSON.stringify(artifact));
    const validated = contracts(["--artifact",temporary]);
    fs.renameSync(temporary,target);
    return {ok:true,artifact:target,digest:validated.digest,definition:validated.definition};
  } finally {fs.rmSync(temporary,{force:true});}
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  try {
    const [command,source,output,...extra] = process.argv.slice(2);
    let result;
    if ((command === "describe" || command === "--help" || command === undefined) && !source) result=describe();
    else if (command === "init" && source && !output) result=init(source);
    else if (command === "check" && source && !output) result={ok:true,...contracts(["--artifact",path.resolve(source)])};
    else if (command === "build" && source && output && !extra.length) result=await build(source,output);
    else throw new Error("Use wes-view-package describe for supported commands");
    console.log(JSON.stringify(result));
  } catch(error) {
    console.log(JSON.stringify({ok:false,diagnostics:diagnostics(error)}));
    process.exitCode = 1;
  }
}
