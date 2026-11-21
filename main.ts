// The harness driver for this entry: the grader runs `node /bot/main.ts` (Node 22). It
// reads every case from /input/cases.json and writes /output/answers.json: a JSON array
// of the answers, one per case, in the cases' order. The fast path never parses the
// envelope in JavaScript and never copies it either: the file is read straight into
// the WebAssembly module's memory (solve.js's staged API) and the answers are written
// straight out of that memory. If the staged path ever throws, the driver falls back
// to solve.js's copying transduceAll, and then to answering case by case with run(),
// so the output is still correct. Outside the grader's container, MAKEFUN_INPUT_DIR
// and MAKEFUN_OUTPUT_DIR stand in for /input and /output.

import { closeSync, fstatSync, openSync, readFileSync, readSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setFlagsFromString } from "node:v8";

// Turn V8's incremental marking off before solve.js is loaded, so the work runs under
// the same condition the house baseline is timed under.
setFlagsFromString("--no-incremental-marking");

// solve.js is a plain JavaScript module; load it by URL and pin its shape to the contract.
const solve = (await import(new URL("./solve.js", import.meta.url).href)) as {
  run: (input: unknown) => unknown;
  transduceAll: (casesBytes: Uint8Array) => Uint8Array;
  inputPtr: (len: number) => number;
  memoryBuffer: () => ArrayBuffer;
  transduceStaged: (len: number) => Uint8Array;
};

const inputDir = process.env.MAKEFUN_INPUT_DIR ?? "/input";
const outputDir = process.env.MAKEFUN_OUTPUT_DIR ?? "/output";
const inputPath = join(inputDir, "cases.json");
const outputPath = join(outputDir, "answers.json");

try {
  // Zero-copy path: read the file directly into the module's input buffer, run
  // the transducer there, and write the answers directly from the module's
  // output buffer.
  const fd = openSync(inputPath, "r");
  let staged = 0;
  try {
    const size = fstatSync(fd).size;
    const ptr = solve.inputPtr(size);
    const view = new Uint8Array(solve.memoryBuffer(), ptr, size);
    while (staged < size) {
      const n = readSync(fd, view, staged, size - staged, staged);
      if (n === 0) break;
      staged += n;
    }
  } finally {
    closeSync(fd);
  }
  writeFileSync(outputPath, solve.transduceStaged(staged));
} catch {
  const raw = readFileSync(inputPath);
  let out: Uint8Array;
  try {
    out = solve.transduceAll(raw);
  } catch {
    const cases = JSON.parse(raw.toString("utf8")) as { id: string; input: unknown }[];
    const answers: unknown[] = [];
    for (const entry of cases) {
      answers.push(solve.run(entry.input));
    }
    out = Buffer.from(JSON.stringify(answers), "utf8");
  }
  writeFileSync(outputPath, out);
}
