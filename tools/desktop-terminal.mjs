/** Build the console entry point from the same checkout as the desktop package.
 * Cargo's artifact record respects shared target directories and target configuration. */
import {execFileSync} from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import {fileURLToPath} from "node:url";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const records = execFileSync("cargo", ["build", "--locked", "--release", "-p", "wes", "--bin", "wes", "--message-format=json"],
  {cwd:root, encoding:"utf8", stdio:["ignore", "pipe", "inherit"], maxBuffer:16*1024*1024});
const artifact = records.trim().split("\n").map(line => JSON.parse(line))
  .find(record => record.reason === "compiler-artifact" && record.target.name === "wes" && record.executable);
if (!artifact || path.extname(artifact.executable).toLowerCase() !== ".exe") {
  throw new Error("The Windows desktop package requires a Windows console executable.");
}
const destination = path.join(root, "crates/desktop/resources/wes-terminal.exe");
fs.mkdirSync(path.dirname(destination), {recursive:true});
fs.copyFileSync(artifact.executable, destination);
