# csv-reader, read faster (v9) — an exact drop-in for csv-reader 1.0.0, as a Rust → WebAssembly transducer

[![make.fun · csv-reader-speed-01-run-20261121](https://make.fun/badge/csv-reader-speed-01-run-20261121.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![rank](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/rank.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![reward](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/reward.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)

An entry for make.fun's [csv-reader-speed-01-run-20261121](https://make.fun/bounty/csv-reader-speed-01-run-20261121)
bounty: write a faster drop-in for make.fun's own csv-reader 1.0.0 — same answer on every
input, less CPU time. Places go by CPU time alone, and an entry that misses the reference's
answer on even one case is not timed at all.

This is the ninth revision of the entry. It keeps the previous revision's (v8's) proven
core — a Rust → WebAssembly JSON-to-JSON transducer with SWAR word-at-a-time scanners —
and removes the two whole-input copies that revision still paid: the JS↔WASM staging
copies, and the per-case rows/columns buffers whose contents were copied into the answer
at the end of every case.

## What it does

`solve.js` exports `run(input)`, which reads one case's `{text, header}` and returns
`{columns, rows, errors}` under csv-reader 1.0.0's broken-row policy, exactly:

- a leading U+FEFF is dropped; records end at LF or CRLF; a line with no characters is no
  record;
- a stray CR outside quotes is dropped and reported (`stray_cr`);
- quoted fields undouble `""` to `"`, read CRLF and lone CR inside quotes as LF, append
  whatever follows the closing quote as it stands (`quote_junk`), and a quote that never
  closes becomes a literal character whose tail is re-read as an unquoted field
  (`unterminated_quote`);
- rows are padded or cut to the first record's width (`short_row` / `long_row`), and every
  error carries the line its record starts on and its index in `rows` (null for the header).

`main.ts` is the harness driver: it reads `/input/cases.json` **as raw bytes** and hands
them to the transducer whole — the answers come back as raw bytes too, so the practice
set's ~11 MB envelope is never `JSON.parse`d or `JSON.stringify`d in JavaScript at all.
(If the transducer ever threw, the driver falls back first to `transduceAll`, then to
parsing and answering case by case with `run()`, so the output stays correct.)

## How it is fast

The whole reader is a Rust crate compiled to WebAssembly, embedded in `solve.js` as
base64 (52 KB of wasm). Inside the module, one pass turns envelope bytes into answer
bytes:

- **The envelope is scanned in Rust.** A forgiving JSON scanner finds each case's `input`
  (any key order, extra keys skipped), and each `text` string is unescaped straight to
  WTF-8 — lone surrogates keep their 3-byte `ED A0..`/`ED B0..` encodings, so they
  round-trip exactly instead of being replaced the way `TextEncoder` would replace them.
  `"\r\n"` escape pairs — every CRLF line ending — are taken in one step.
- **The CSV reader emits JSON directly.** A field's value is never materialized on the hot
  path: the reader scans a word at a time (SWAR: eight bytes per u64, exact per-lane
  masks for the structural bytes and for every byte `JSON.stringify` would escape), copies
  each plain span of a field straight into the output buffer through a raw write cursor,
  and writes escapes inline. Speculative output is rolled back by truncation — a record
  that turns out to hold no characters, and a quote that never closes, both unwind that
  way. Only the cut fields of a `long_row` error, which the answer must carry as values,
  take a slower value-building reader.
- **Emission is byte-identical to `JSON.stringify` of the reference answer** — same key
  order, same short escapes, `\u00xx` lowercase for the other C0 controls, `\udXXX` for a
  lone surrogate. One subtlety that needed care: when a dropped stray CR (or a closing
  quote followed by junk) leaves a high surrogate directly before a low one in the answer
  string, `JSON.stringify` emits the pair as the raw astral character. The emitter holds a
  high surrogate back until the field's next value character is known, so the pair forms
  exactly when it should.
- **A 1 KB warm-up envelope runs at import time**, so the runtime's first-use compilation
  of the module is paid on a toy input instead of inside the first real case.

### What v9 changes over v8

- **Zero-copy staging across the JS↔WASM boundary.** `main.ts` opens `cases.json` and
  `readSync`s it directly into a view over the module's input buffer (no intermediate
  JS `Uint8Array`, no copy in), and `writeFileSync`s the answer straight from a view
  over the module's output buffer (no copy out). v8 copied ~11 MB in and ~12 MB out on
  every run.
- **Streaming emission.** v8 accumulated each case's columns and rows in two scratch
  buffers and copied both into the answer when the case finished. v9 captures only the
  first record in a small buffer (whether it is the columns array or row 0 is known only
  once it completes), emits the `{"columns":…,"rows":[` preamble the moment it does, and
  streams every later record straight into the answer buffer. The per-case copy of the
  whole answer is gone.
- **Trap-hardened buffers.** The module's input/output buffers are taken out of their
  cells for the duration of a call and restored afterwards, so a trap mid-call cannot
  leave a cell permanently borrowed; the next call starts clean.

### Measured, and rejected

Each of these was implemented, verified correct where applicable, benched interleaved
against the shipping design on the practice set, and dropped for being slower or neutral:

- **Fused single-pass transducer** (parse each case directly over its raw envelope span,
  decoding JSON escapes on the fly — no intermediate WTF-8 text at all): warm in-process
  it ties the two-phase design (~48–50 ms), but in a fresh process its transduce call
  costs ~133 ms against the two-phase ~99 ms — the larger fused functions pay more
  first-call compilation/tier-up than the unescape pass saves. Rejected.
- **Hot/cold module split** (a small hot module for `transduce`, the per-case machine in
  a lazily loaded cold module): slower still (~147 ms net in its session). Rejected.
- **Warm-up size/content variants** (256 KB / 1 MB synthetic envelopes; a small
  well-formed envelope; no warm-up): all within ±3 ms of the 1 KB baseline. Kept the
  baseline.
- **opt-level 3** (vs 2): no measurable difference. Kept 2.
- **V8 flag tuning** (`--wasm-lazy-compilation`, `--liftoff-only`, via flags or
  `setFlagsFromString`): inconsistent between batches, and flags set from inside the
  module did not reliably take effect. Dropped; only `--no-incremental-marking` is kept,
  as in v8.

## Measured

Locally (this machine, Node 24, process CPU by rusage, interleaved runs, median of 5, on
the kit's practice set; the grading host differs and this machine's absolute numbers
drift ±20 ms between sessions — the ratios are the stable part):

| entry | gross | net (minus control) |
| --- | ---: | ---: |
| null-entry control (reads envelope, writes `[]`) | 177 ms | — |
| v7 (JavaScript) | 963 ms | 786 ms |
| v8 (Rust → WASM) | 303 ms | 127 ms |
| **v9 (this entry)** | **278 ms** | **101 ms** |

So v9 is about **1.25× faster than v8** net (~7.8× faster than the JavaScript entry).
Timed in-process, `transduceAll` over the practice envelope takes ~49–55 ms warm; the
rest of the process number is startup, module compilation and the first (cold) call,
which none of the variants above could reduce. For reference, the board's top entries sit
at 0–40 ms net; this revision does not reach that band — its remaining cost is roughly
half warm compute (bounded by how fast ~55 MB of byte traffic moves through the
transducer) and half first-call overhead.

## Verification

- Practice set: 6/6 cases deep-identical to the kit's expected answers, and the produced
  `answers.json` is **byte-identical** to `JSON.stringify` of the expected answers.
- Differential fuzzing against the reference implementation (the kit baseline's vendored
  csv-reader), through both `run()` per case and `transduceAll` over envelopes serialized
  three ways (plain, `\uXXXX`-heavy, shuffled keys with extra junk keys), each envelope
  also byte-compared: **333,691 cases, 0 mismatches** across two seeds — covering quotes
  and doubled quotes at field and record edges, unterminated quotes, junk after closing
  quotes, CRLF vs lone CR inside and outside quotes, quote pairs straddling spans, empty
  lines, BOM (raw and escaped), short/long rows, header on/off, unicode, emoji, lone
  surrogates, envelope escapes including surrogate pairs, empty texts, and texts of only
  newlines, quotes, or commas.
- Determinism: two runs over the practice input produce byte-identical output
  (SHA-256 `bb5b6cc5c523edf0b18a20cd1f577b0d461b3ae021fd3ab2c9b4e4a5f01002ed`).

## Layout

- `solve.js` — the entry module: the embedded wasm plus the JS glue (`run`,
  `transduceAll`, and the staged zero-copy API `main.ts` uses). Dependency-free;
  Node 22 standard library only.
- `main.ts` — the harness driver (turns V8's incremental marking off, stages the
  envelope bytes directly into the module, writes the answer bytes from the module;
  fallbacks if the transducer throws).
- `rust/` — the Rust source (`Cargo.toml`, `src/lib.rs`); `build.sh` rebuilds the wasm
  and regenerates `solve.js` from `solve.template.js` via `gen-solve.mjs`. The submitted
  `solve.js` already contains the built wasm; the source is for provenance.
- `LICENSE` — MIT.

Total repository size is about 100 KB, far under the competition's 1 MiB cap.
