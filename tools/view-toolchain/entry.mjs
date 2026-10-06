#!/usr/bin/env node
/** Run the exported compiler with its matching native validator. No installation or downloads. */
import path from "node:path";
import fs from "node:fs";
import {fileURLToPath, pathToFileURL} from "node:url";
const root = path.dirname(fileURLToPath(import.meta.url));
const kit = JSON.parse(fs.readFileSync(path.join(root,"toolchain.json"),"utf8"));
const os = ({darwin:"macos",win32:"windows"})[process.platform] ?? process.platform;
const arch = ({arm64:"aarch64",x64:"x86_64",ia32:"x86"})[process.arch] ?? process.arch;
if (!process.env.WES_VIEW_CONTRACT_TOOL && (kit.os !== os || kit.arch !== arch)) {
  throw new Error("This exported validator belongs to another platform. Export a matching kit or explicitly set WES_VIEW_CONTRACT_TOOL to a compatible validator.");
}
process.env.WES_VIEW_CONTRACT_TOOL ??= path.join(root,"bin",kit.validator);
const entry = path.join(root,"compiler","index.mjs");
process.argv[1] = entry;
await import(pathToFileURL(entry).href);
