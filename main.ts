// The harness driver for this entry: the grader runs `node /bot/main.ts` (Node 22). It
// reads every case from /input/cases.json and writes /output/answers.json: a JSON array
// of the answers, one per case, in the cases' order. The fast path never parses the
// envelope in JavaScript: the raw cases.json bytes go to solve.js's transduceAll, whose
// WebAssembly module unescapes, reads and serializes everything itself. If that path
// ever throws, the driver falls back to answering case by case with run(), so the
// output is still correct. Outside the grader's container, MAKEFUN_INPUT_DIR and
// MAKEFUN_OUTPUT_DIR stand in for /input and /output.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setFlagsFromString } from "node:v8";

// Turn V8's incremental marking off before solve.js is loaded, so the work runs under
// the same condition the house baseline is timed under.
setFlagsFromString("--no-incremental-marking");

// solve.js is a plain JavaScript module; load it by URL and pin its shape to the contract.
const solve = (await import(new URL("./solve.js", import.meta.url).href)) as {
  run: (input: unknown) => unknown;
  transduceAll: (casesBytes: Uint8Array) => Uint8Array;
};

const inputDir = process.env.MAKEFUN_INPUT_DIR ?? "/input";
const outputDir = process.env.MAKEFUN_OUTPUT_DIR ?? "/output";

const raw = readFileSync(join(inputDir, "cases.json"));

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

writeFileSync(join(outputDir, "answers.json"), out);
