# csv-reader, read faster — an exact drop-in for csv-reader 1.0.0, as a Rust → WebAssembly transducer

[![make.fun · csv-reader-speed-01-run-20261121](https://make.fun/badge/csv-reader-speed-01-run-20261121.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![rank](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/rank.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)
[![reward](https://make.fun/badge/csv-reader-speed-01-run-20261121/jontumpil/makefun-csv-reader/reward.svg)](https://make.fun/bounty/csv-reader-speed-01-run-20261121?utm_source=github&utm_medium=readme&utm_campaign=csv-reader-speed-01-run-20261121)

An entry for make.fun's [csv-reader-speed-01-run-20261121](https://make.fun/bounty/csv-reader-speed-01-run-20261121)
bounty: write a faster drop-in for make.fun's own csv-reader 1.0.0 — same answer on every
input, less CPU time. Places go by CPU time alone, and an entry that misses the reference's
answer on even one case is not timed at all.

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
(If the transducer ever threw, the driver falls back to parsing and answering case by case
with `run()`, so the output stays correct.)

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

Measured locally (this machine, Node 24, process CPU by rusage, median of 5 runs on the
kit's practice set; the grading host differs): the null-entry control (reads the envelope,
writes `[]`) costs ~165 ms of process CPU, the previous JavaScript entry ~1,040 ms gross
(~880 ms net), and this entry ~310 ms gross (**~145 ms net, about 6× faster**). The
transducer itself, timed in-process on the practice envelope, takes ~80 ms on its first
call in a fresh process and ~55 ms warm.

## Verification

- Practice set: 6/6 cases deep-identical to the kit's expected answers, and the produced
  `answers.json` is **byte-identical** to `JSON.stringify` of the expected answers.
- Differential fuzzing against the reference implementation (the kit baseline's vendored
  csv-reader), through both `run()` per case and `transduceAll` over envelopes serialized
  three ways (plain, `\uXXXX`-heavy, shuffled keys with extra junk keys): **364,792 cases,
  0 mismatches** — covering quotes and doubled quotes, unterminated quotes, junk after
  closing quotes, CRLF vs lone CR inside and outside quotes, empty lines, BOM, short/long
  rows, header on/off, unicode and emoji, envelope escapes including surrogate pairs and
  lone surrogates, empty texts, and texts of only newlines, quotes, or commas.
- Determinism: two runs over the practice input produce byte-identical output
  (SHA-256 `bb5b6cc5c523edf0b18a20cd1f577b0d461b3ae021fd3ab2c9b4e4a5f01002ed`).

## Layout

- `solve.js` — the entry module: the embedded wasm plus the JS glue (`run`,
  `transduceAll`). Dependency-free; Node 22 standard library only.
- `main.ts` — the harness driver (turns V8's incremental marking off, reads the envelope
  as bytes, writes the answer bytes; per-case fallback if the transducer throws).
- `rust/` — the Rust source (`Cargo.toml`, `src/lib.rs`); `build.sh` rebuilds the wasm
  and regenerates `solve.js` from `solve.template.js` via `gen-solve.mjs`. The submitted
  `solve.js` already contains the built wasm; the source is for provenance.
- `LICENSE` — MIT.

Total repository size is about 100 KB, far under the competition's 1 MiB cap.
