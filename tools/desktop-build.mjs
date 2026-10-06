/** Native packaging prerequisites. This builds local assets, never publishes a release. */
import {execFileSync} from "node:child_process";
import {fileURLToPath} from "node:url";
const root = fileURLToPath(new URL("../", import.meta.url));
execFileSync("cargo", ["build","--release","--locked","-p","wes-views","--bin","wes-view-build"], {cwd:root,stdio:"inherit"});
execFileSync(process.platform === "win32" ? "npm.cmd" : "npm", ["run","build"], {cwd:root,stdio:"inherit",shell:process.platform === "win32"});
