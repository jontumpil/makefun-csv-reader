// Generates solve.js from solve.template.js by embedding the compiled WebAssembly
// module (rust/target/wasm32-unknown-unknown/release/csv_reader_v13s.wasm) as base64.
// Run by build.sh after the cargo build; solve.js is committed with the wasm inside.
import { readFileSync, writeFileSync } from "node:fs";

const wasm = readFileSync(
  new URL("./rust/target/wasm32-unknown-unknown/release/csv_reader_v13s.wasm", import.meta.url),
);
const template = readFileSync(new URL("./solve.template.js", import.meta.url), "utf8");
if (!template.includes("__WASM_B64__")) {
  throw new Error("solve.template.js holds no __WASM_B64__ marker");
}
const solve = template.replace("__WASM_B64__", wasm.toString("base64"));
writeFileSync(new URL("./solve.js", import.meta.url), solve);
console.log(`solve.js written: ${solve.length} bytes (wasm ${wasm.length} bytes)`);
