# csv-reader, read faster — an exact drop-in for csv-reader 1.0.0, built for speed

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

`main.ts` is the harness driver: it reads `/input/cases.json`, answers each case with
`run()`, and writes `/output/answers.json`.

## How it is fast

The reference reader walks the text one character at a time and grows each field one
character at a time. This reader scans ahead to the next character that can change anything
(a comma, a quote, a CR or an LF) and copies each plain stretch of a field with a single
`slice`, so clean text costs one scan and one slice per field. The broken corners — stray
CRs, doubled quotes, CRs inside quotes, junk after a closing quote, quotes that never
close — take dedicated paths that follow the policy step for step, and the whole reader was
verified two ways before it was entered: against the kit's practice set (6/6 cases
identical to the reference's expected answers) and against the reference program itself on
a large fuzz corpus of randomly broken CSV texts (zero mismatches).

## Layout

- `solve.js` — the reader (dependency-free ES module, Node 22 standard library only).
- `main.ts` — the harness driver (turns V8's incremental marking off before loading
  `solve.js`, as the house baseline does).
- `LICENSE` — MIT.

Total repository size is a few kilobytes, far under the competition's 1 MiB cap.
