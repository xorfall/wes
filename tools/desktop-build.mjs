/** One owner for the native programs and GUI assets included in the desktop package. */
import {execFileSync} from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import {fileURLToPath} from "node:url";
const repository = fileURLToPath(new URL("../", import.meta.url));
export function buildDesktop({root=repository, platform=process.platform, env=process.env, run=execFileSync}={}) {
  const target = env.TAURI_ENV_TARGET_TRIPLE || env.CARGO_BUILD_TARGET;
  const windows = target ? target.includes("-windows-") : platform === "win32";
  const programs = [{name:"wes-view-build", package:"wes-views", destination:windows ? "wes-view-build.exe" : "wes-view-build"}];
  if (windows) programs.push({name:"wes", package:"wes", destination:"wes-terminal.exe"});
  const args = ["build", "--locked", "--release", "--message-format=json"];
  if (target) {
    const version = run("rustc", ["-vV"], {cwd:root,env,encoding:"utf8"});
    const host = /^host: (.+)$/m.exec(version)?.[1]?.trim();
    if (!host) throw new Error("The desktop build could not identify the Rust host target.");
    // Tauri announces the host triple even for native builds. An unnecessary --target
    // creates a second Cargo cache; an explicit conflicting Cargo default still needs override.
    if (target !== host || (env.CARGO_BUILD_TARGET && env.CARGO_BUILD_TARGET !== target)) args.push("--target", target);
  }
  for (const program of programs) args.push("-p", program.package, "--bin", program.name);
  const output = run("cargo", args, {cwd:root, env, encoding:"utf8", stdio:["ignore","pipe","inherit"], maxBuffer:32*1024*1024});
  const records = output.trim().split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line));
  const artifacts = programs.map(program => {
    const matches = records.filter(record => record.reason === "compiler-artifact" && record.target?.name === program.name && record.target?.kind?.includes("bin") && record.executable);
    if (matches.length !== 1 || path.extname(matches[0].executable).toLowerCase() !== (windows ? ".exe" : "") || !fs.statSync(matches[0].executable).isFile()) {
      throw new Error(`The desktop build requires one matching native ${program.name} executable.`);
    }
    return {...program, source:matches[0].executable};
  });
  const resources = path.join(root, "crates/desktop/resources");
  fs.mkdirSync(resources, {recursive:true});
  for (const artifact of artifacts) {
    const destination = path.join(resources, artifact.destination);
    const pending = `${destination}.pending-${process.pid}`;
    fs.copyFileSync(artifact.source, pending, fs.constants.COPYFILE_EXCL);
    try { fs.renameSync(pending, destination); }
    finally { if (fs.existsSync(pending)) fs.unlinkSync(pending); }
  }
  // These fixed build arguments contain no user shell input. npm is a batch program on Windows.
  run(platform === "win32" ? "npm.cmd" : "npm", ["run","build"], {cwd:root, env, stdio:"inherit", shell:platform === "win32"});
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) buildDesktop();
