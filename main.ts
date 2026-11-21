// The harness driver for this entry: the grader runs `node /bot/main.ts` (Node 22). It reads
// every case from /input/cases.json (a JSON array of {id, input}), answers each case on its
// own with run() from ./solve.js, and writes /output/answers.json: a JSON array of the
// answers, one per case, in the cases' order. Outside the grader's container,
// MAKEFUN_INPUT_DIR and MAKEFUN_OUTPUT_DIR stand in for /input and /output.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setFlagsFromString } from "node:v8";

// Turn V8's incremental marking off before solve.js is loaded, so the parser's work runs
// under the same condition the house baseline is timed under.
setFlagsFromString("--no-incremental-marking");

// solve.js is a plain JavaScript module; load it by URL and pin its shape to the contract.
const { run } = (await import(new URL("./solve.js", import.meta.url).href)) as {
  run: (input: unknown) => unknown;
};

const inputDir = process.env.MAKEFUN_INPUT_DIR ?? "/input";
const outputDir = process.env.MAKEFUN_OUTPUT_DIR ?? "/output";

const cases = JSON.parse(readFileSync(join(inputDir, "cases.json"), "utf8")) as {
  id: string;
  input: unknown;
}[];

const answers: unknown[] = [];
for (const entry of cases) {
  answers.push(run(entry.input));
}

writeFileSync(join(outputDir, "answers.json"), JSON.stringify(answers));
