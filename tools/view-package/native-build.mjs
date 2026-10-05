/** Cargo supplies metadata already checked by the same native Package parser.
 * Compile a private source copy; generated declarations never modify the checkout. */
import fs from "node:fs";
import path from "node:path";
async function main() {
  const {compile} = await import("./index.mjs");
  const [source,metadataFile,target] = process.argv.slice(2);
  const working = path.join(path.dirname(target),"source");
  fs.rmSync(working,{recursive:true,force:true});
  fs.cpSync(source,working,{recursive:true});
  const artifact = await compile(working,JSON.parse(fs.readFileSync(metadataFile,"utf8")));
  fs.writeFileSync(target,JSON.stringify(artifact));
}
await main().catch(error => {
  if (error.code === "MODULE_NOT_FOUND" || error.code === "ERR_MODULE_NOT_FOUND") {
    console.error("Shipped View compiler dependencies are unavailable. Run npm ci from the Wes repository root before cargo build.");
    console.error(error.message);
    process.exit(1);
  }
  throw error;
});
