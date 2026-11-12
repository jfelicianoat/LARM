import { createHash } from "node:crypto";
import { readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const inputs = ["package.json", "index.html", "vite.config.ts", "tsconfig.json", "tsconfig.node.json", "tsconfig.app.json", "build-ui-stamp.mjs"];
async function files(dir) {
  const names = [];
  for (const entry of await readdir(path.join(root, dir), { withFileTypes: true })) {
    const relative = `${dir}/${entry.name}`;
    if (entry.isDirectory()) names.push(...await files(relative));
    else if (entry.isFile() && relative !== "dist/ui-build.json") names.push(relative);
  }
  return names.sort();
}
async function hashes(names) {
  const result = {};
  for (const name of names.sort()) result[name] = createHash("sha256").update(await readFile(path.join(root, name))).digest("hex");
  return result;
}
const manifest = { schema_version: 1, sources: await hashes([...inputs, ...await files("src")]), assets: await hashes(await files("dist")) };
await writeFile(path.join(root, "dist/ui-build.json"), `${JSON.stringify(manifest, null, 2)}\n`);
