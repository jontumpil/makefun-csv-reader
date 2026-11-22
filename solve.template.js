// A fast reader for broken CSV text: csv-reader 1.0.0's semantics compiled from Rust to
// WebAssembly, driven from JavaScript.
//
// run(input) answers one case's input {text, header} with {columns, rows, errors}, following
// csv-reader 1.0.0's reading policy exactly:
//
//   - a U+FEFF at the very start of the text is dropped;
//   - a record ends at LF or CRLF, and a line left with no characters is no record;
//   - outside quotes, a CR not followed by LF is dropped and reported as stray_cr;
//   - a field that opens with a quote runs to the next quote that is not doubled: "" inside
//     it is one quote, and each CRLF or lone CR inside it reads as one LF; whatever follows
//     the closing quote, up to the next comma or line break, is appended as it stands
//     (quote_junk, reported when the field ends, if it appended anything); a quote that
//     never closes is a literal character and the text after it is read again as an
//     unquoted field (unterminated_quote);
//   - a quote inside an unquoted field is a literal character; fields stay strings, never
//     trimmed or converted;
//   - columns is the first record when header is true, else null; rows holds the records
//     after it (every record when header is false), each padded with empty strings
//     (short_row, with fields: its own count) or cut (long_row, with extra: the cut
//     fields) to the first record's width;
//   - each error is {code, line, row}: line is the 1-based line the record starts on,
//     counting every LF, CRLF and lone CR inside quotes; row is the record's index in
//     rows, null for the header; errors come in the order met, a record's short_row or
//     long_row when it ends.
//
// The reference program walks the text one character at a time in JavaScript and the
// previous version of this entry scanned in JavaScript too; both spend most of their time
// in JSON.parse, string concatenation and JSON.stringify. This version keeps all of that
// inside one WebAssembly module (its Rust source is under rust/): the module takes the
// raw bytes of the whole cases.json envelope, unescapes each case's text itself, reads
// the CSV with 16-byte SIMD classification of the structural bytes, and returns the raw
// bytes of the answers array, serialized exactly as JSON.stringify of the reference
// answers. Texts travel between JavaScript and the module as WTF-8, so a lone surrogate
// in a text survives the trip and comes back as its \udXXX escape, exactly as
// JSON.stringify of the reference answer writes it.
//
// transduceAll(casesBytes) is the hot path main.ts uses; run(input) answers a single
// case through the same module and parses its answer bytes back into a value.


const WASM_B64 = "__WASM_B64__";

function decodeBase64(b64) {
  if (typeof Buffer !== "undefined") return Buffer.from(b64, "base64");
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

const wasm = await WebAssembly.instantiate(decodeBase64(WASM_B64), {});
const wasmExports = wasm.instance.exports;
const wasmMemory = wasmExports.memory;

// Runs one exported function over `data`: stages the bytes in the module's input buffer,
// calls, and copies the packed (pointer, length) result out of the module's memory.
// Memory is re-read after every call because a call may grow it.
function callWasm(fn, data, arg) {
  const ptr = wasmExports.in_ptr(data.length);
  new Uint8Array(wasmMemory.buffer, ptr, data.length).set(data);
  const packed = arg === undefined ? fn(data.length) : fn(data.length, arg);
  const outPtr = Number(packed & 0xffffffffn);
  const outLen = Number((packed >> 32n) & 0xffffffffn);
  return new Uint8Array(wasmMemory.buffer, outPtr, outLen).slice();
}

/** The raw answers.json bytes for a whole cases.json envelope, without parsing it in JS. */
export function transduceAll(casesBytes) {
  return callWasm(wasmExports.transduce, casesBytes);
}

/** A writable pointer for `len` input bytes staged directly in the module's memory. */
export function inputPtr(len) {
  return wasmExports.in_ptr(len);
}

/** The module's current memory buffer. It is replaced whenever the memory grows,
 * so read it after every call into the module, never cache it across one. */
export function memoryBuffer() {
  return wasmMemory.buffer;
}

/**
 * Transduces the `len` bytes already staged through inputPtr and returns a VIEW
 * of the answers bytes inside the module's memory — no copy out. The view is
 * invalidated by the next call into the module; consume it (for example write
 * it to a file) before calling anything else in this module.
 */
export function transduceStaged(len) {
  const packed = wasmExports.transduce(len);
  const outPtr = Number(packed & 0xffffffffn);
  const outLen = Number((packed >> 32n) & 0xffffffffn);
  return new Uint8Array(wasmMemory.buffer, outPtr, outLen);
}

// One tiny representative envelope through the transducer before anything
// real: the runtime compiles the module's functions on first use and starts
// optimizing the hot ones in the background, and doing that on 1 KB now beats
// paying all of it inside the first real call. The result is discarded.
try {
  const warm = new TextEncoder().encode(
    '[{"id":"w","input":{"text":"a,b,\\"c\\nd\\",e\\r\\n1,2,\\"x\\"\\"y\\"junk,3\\n' +
      'a,b,\\"c\\nd\\",e\\r\\n1,2,\\"x\\"\\"y\\"junk,3\\n","header":true}}]',
  );
  transduceAll(warm);
} catch {
  // A warm-up failure must never break the module; the real call retries cold.
}

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

// Encodes a JS string as WTF-8: TextEncoder already does exactly this except that it
// replaces lone surrogates with U+FFFD, so strings holding one take the manual path,
// which encodes a lone surrogate as its 3-byte ED A0.. / ED B0.. sequence.
function encodeWtf8(str) {
  let hasSurrogate = false;
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    if (c >= 0xd800 && c <= 0xdfff) {
      hasSurrogate = true;
      break;
    }
  }
  if (!hasSurrogate) return textEncoder.encode(str);
  const out = [];
  for (let i = 0; i < str.length; i++) {
    const cu = str.charCodeAt(i);
    if (cu >= 0xd800 && cu <= 0xdbff && i + 1 < str.length) {
      const lo = str.charCodeAt(i + 1);
      if (lo >= 0xdc00 && lo <= 0xdfff) {
        const cp = 0x10000 + ((cu - 0xd800) << 10) + (lo - 0xdc00);
        out.push(
          0xf0 | (cp >> 18),
          0x80 | ((cp >> 12) & 0x3f),
          0x80 | ((cp >> 6) & 0x3f),
          0x80 | (cp & 0x3f),
        );
        i++;
        continue;
      }
    }
    if (cu < 0x80) out.push(cu);
    else if (cu < 0x800) out.push(0xc0 | (cu >> 6), 0x80 | (cu & 0x3f));
    else
      out.push(
        0xe0 | (cu >> 12),
        0x80 | ((cu >> 6) & 0x3f),
        0x80 | (cu & 0x3f),
      );
  }
  return Uint8Array.from(out);
}

/** csv-reader 1.0.0's answer for one case's input {text, header}: {columns, rows, errors}. */
export function run(input) {
  const text = typeof input.text === "string" ? input.text : String(input.text);
  const header = input.header === true ? 1 : 0;
  const answerBytes = callWasm(wasmExports.run_one, encodeWtf8(text), header);
  return JSON.parse(textDecoder.decode(answerBytes));
}
