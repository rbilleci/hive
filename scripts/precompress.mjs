import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

// Writes `.br` and `.gz` siblings for every compressible file of a console build. The server
// (`crates/hive-api/src/spa.rs`) sends the sibling a client accepts, so compression costs nothing per
// request and can run at the highest quality. Usage: node scripts/precompress.mjs <dist directory>
const root = process.argv[2];
if (!root) throw new Error("Name the build directory to compress.");
const compressible = /\.(?:html|js|mjs|css|wasm|svg|json|map|txt)$/;

function files(directory) {
  return readdirSync(directory).flatMap((entry) => {
    const path = join(directory, entry);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}

let raw = 0, brotli = 0, count = 0;
for (const path of files(root)) {
  if (!compressible.test(path)) continue;
  const bytes = readFileSync(path);
  const br = brotliCompressSync(bytes, { params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: bytes.length } });
  writeFileSync(path + ".br", br);
  writeFileSync(path + ".gz", gzipSync(bytes, { level: 9 }));
  raw += bytes.length; brotli += br.length; count += 1;
}
console.log(`precompress: ${count} files in ${root}, ${Math.round(raw / 1000)} kB raw, ${Math.round(brotli / 1000)} kB brotli.`);
