//! csv-reader 1.0.0's semantics as a Rust -> WebAssembly JSON-to-JSON transducer.
//!
//! The module never exposes CSV values to JavaScript on the hot path: `transduce` takes
//! the raw bytes of a cases.json envelope ([{"id", "input": {"text", "header"}}, ...])
//! and returns the raw bytes of the answers array, each answer exactly the value
//! csv-reader 1.0.0's readCsv returns for that case, serialized exactly as
//! JSON.stringify would serialize it. `run_one` does the same for a single case's text
//! (used by solve.js's run()). Texts travel as WTF-8: a lone surrogate is its 3-byte
//! ED A0.. / ED B0.. sequence, so it round-trips byte for byte and is emitted as a
//! \udXXX escape, exactly like JSON.stringify of the reference answer — except when a
//! high surrogate stands directly before a low one in the answer string (two lone
//! surrogates can end up adjacent that way when a dropped stray CR stood between them
//! in the text), where JSON.stringify emits the pair as the raw astral character and
//! so does this module.
//!
//! The reading policy is the reference's, step for step (see ../solve.template.js and
//! the kit's vendor/csv-reader/index.js): a leading U+FEFF is dropped; records end at
//! LF or CRLF and a line with no characters is no record; a CR outside quotes not
//! followed by LF is dropped (stray_cr); a field opening with a quote runs to the next
//! undoubled quote ("" is one quote, CRLF and lone CR inside read as LF), what follows
//! the closing quote up to the next comma or line break is appended as it stands
//! (quote_junk when it appended anything); a quote that never closes is a literal
//! character and the text after it is re-read as unquoted (unterminated_quote); a
//! quote inside an unquoted field is literal; rows are padded (short_row, with
//! fields: the count) or cut (long_row, with extra: the cut fields) to the first
//! record's width; errors are {code, line, row} in the order met, line the 1-based
//! line the record starts on counting every LF, CRLF and lone CR inside quotes, row
//! the record's index in rows, null for the header.
//!
//! Speed comes from writing the answer JSON in a single pass: the field readers scan
//! a u64 word at a time (SWAR), classifying each word for the few bytes
//! that can change anything (the structural bytes plus every byte JSON.stringify
//! would escape), copy each plain span of a field straight into the output, and emit
//! escapes inline — a field's value is never materialized on the hot path. The two
//! places that can still need a value — the cut fields of a long_row error, and
//! nothing else — take a slower value-building reader. A speculative output can be
//! rolled back by truncation: a record that turns out to hold no characters (a line
//! of dropped stray CRs) is unwound from the output, and a quote that never closes
//! unwinds its field before the re-read as an unquoted field.

use std::arch::wasm32::*;
use std::cell::RefCell;

thread_local! {
    static IN: RefCell<Vec<u8>> = RefCell::new(Vec::new());
    static OUT: RefCell<Vec<u8>> = RefCell::new(Vec::new());
}

/// Hands JavaScript a writable pointer for `len` input bytes; the bytes must be
/// written before the next call into this module.
#[no_mangle]
pub extern "C" fn in_ptr(len: u32) -> u32 {
    IN.with(|b| {
        let mut b = b.borrow_mut();
        b.clear();
        b.resize(len as usize, 0);
        b.as_mut_ptr() as usize as u32
    })
}


/// Transduces a whole cases.json envelope held in the input buffer. Returns the
/// packed (pointer, length) of the answers.json bytes in the output buffer.
/// The buffers are taken out of their cells for the call (and put back after),
/// so a trap mid-call cannot leave a cell permanently borrowed: the next call
/// simply starts from empty buffers, which the caller's staging refills anyway.
#[no_mangle]
pub extern "C" fn transduce(len: u32) -> u64 {
    let input = IN.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let mut out = OUT.with(|b| std::mem::take(&mut *b.borrow_mut()));
    out.clear();
    out.reserve(input.len());
    transduce_all(&input[..len as usize], &mut out);
    let packed = (out.as_ptr() as usize as u64) | ((out.len() as u64) << 32);
    OUT.with(|b| *b.borrow_mut() = out);
    IN.with(|b| *b.borrow_mut() = input);
    packed
}

/// Answers one case: the input buffer holds the case text as WTF-8 bytes, `header`
/// is nonzero when the case's header flag is true. Returns the packed (pointer,
/// length) of the answer's JSON bytes. Buffers are taken and restored, as in
/// `transduce`.
#[no_mangle]
pub extern "C" fn run_one(len: u32, header: u32) -> u64 {
    let input = IN.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let mut out = OUT.with(|b| std::mem::take(&mut *b.borrow_mut()));
    out.clear();
    let mut bu = Bu::default();
    let mut tr = Tr::new(&input[..len as usize], header != 0, &mut bu, &mut out);
    tr.run();
    let packed = (out.as_ptr() as usize as u64) | ((out.len() as u64) << 32);
    OUT.with(|b| *b.borrow_mut() = out);
    IN.with(|b| *b.borrow_mut() = input);
    packed
}

// ---------------------------------------------------------------------------
// Scanners: index of the first byte at or after `i` that belongs to the
// scanner's set, or the slice length when there is none. They work a u64 at a
// time (SWAR): on this runtime a word-at-a-time scan is several times faster
// than the wasm v128 equivalent, whose bitmask instruction compiles to a long
// emulation sequence. Every mask below is exact — no cross-lane borrows: the
// equality test is the classic zero-byte detector, and "below 0x20" is tested
// as the complement of "at least 0x20", computed per lane as (byte & 0x7F) +
// 0x60, whose sum can never carry into the next lane.
// ---------------------------------------------------------------------------

const SW_ONES: u64 = 0x0101_0101_0101_0101;
const SW_HIGHS: u64 = 0x8080_8080_8080_8080;

#[inline]
fn sw_word(t: &[u8], i: usize) -> u64 {
    unsafe { (t.as_ptr().add(i) as *const u64).read_unaligned() }
}

/// High bit set in every lane whose byte equals `b`.
#[inline]
fn sw_eq(w: u64, b: u8) -> u64 {
    let x = w ^ (SW_ONES.wrapping_mul(b as u64));
    x.wrapping_sub(SW_ONES) & !x & SW_HIGHS
}

/// High bit set in every lane whose byte is below 0x20. The `!w` term keeps
/// bytes at or above 0x80 out: masking them to 7 bits for the addition would
/// otherwise alias 0x80..0x9F onto 0x00..0x1F.
#[inline]
fn sw_lt20(w: u64) -> u64 {
    let ge20 = ((w & 0x7F7F_7F7F_7F7F_7F7F) + 0x6060_6060_6060_6060) & SW_HIGHS;
    !ge20 & !w & SW_HIGHS
}

/// Index of the first lane of `m` with its high bit set, as a byte offset.
#[inline]
fn sw_first(m: u64) -> usize {
    (m.trailing_zeros() / 8) as usize
}

/// Structural bytes of an unquoted field: ',' '\n' '\r'.
#[inline]
fn find_plain(t: &[u8], mut i: usize) -> usize {
    let n = t.len();
    while i + 8 <= n {
        let w = sw_word(t, i);
        let m = sw_eq(w, b',') | sw_eq(w, b'\n') | sw_eq(w, b'\r');
        if m != 0 {
            return i + sw_first(m);
        }
        i += 8;
    }
    while i < n {
        let c = t[i];
        if c == b',' || c == b'\n' || c == b'\r' {
            return i;
        }
        i += 1;
    }
    n
}

/// Structural bytes inside a quoted field: '"' '\n' '\r'.
#[inline]
fn find_quoted(t: &[u8], mut i: usize) -> usize {
    let n = t.len();
    while i + 8 <= n {
        let w = sw_word(t, i);
        let m = sw_eq(w, b'"') | sw_eq(w, b'\n') | sw_eq(w, b'\r');
        if m != 0 {
            return i + sw_first(m);
        }
        i += 8;
    }
    while i < n {
        let c = t[i];
        if c == b'"' || c == b'\n' || c == b'\r' {
            return i;
        }
        i += 1;
    }
    n
}

/// Bytes that end a JSON string or start an escape: '"' '\\'.
#[inline]
fn find_unesc(t: &[u8], mut i: usize) -> usize {
    let n = t.len();
    while i + 8 <= n {
        let w = sw_word(t, i);
        let m = sw_eq(w, b'"') | sw_eq(w, b'\\');
        if m != 0 {
            return i + sw_first(m);
        }
        i += 8;
    }
    while i < n {
        let c = t[i];
        if c == b'"' || c == b'\\' {
            return i;
        }
        i += 1;
    }
    n
}

/// First byte that JSON.stringify would not emit raw: a quote, a backslash, a C0
/// control, or 0xED (the lead byte of U+D000..U+DFFF in UTF-8 and of every lone
/// surrogate in WTF-8; the caller checks whether it really starts a lone surrogate).
#[inline]
fn find_esc(t: &[u8], mut i: usize) -> usize {
    let n = t.len();
    while i + 8 <= n {
        let w = sw_word(t, i);
        let m = sw_eq(w, b'"') | sw_eq(w, b'\\') | sw_lt20(w) | sw_eq(w, 0xED);
        if m != 0 {
            return i + sw_first(m);
        }
        i += 8;
    }
    while i < n {
        let c = t[i];
        if c == b'"' || c == b'\\' || c < 0x20 || c == 0xED {
            return i;
        }
        i += 1;
    }
    n
}

/// Events of the direct-emitting unquoted reader: the structural bytes plus every
/// byte JSON.stringify would escape ('\n' and '\r' ride along as C0 controls and
/// are told apart by the caller).
#[inline]
fn find_event_plain(t: &[u8], mut i: usize) -> usize {
    let n = t.len();
    while i + 8 <= n {
        let w = sw_word(t, i);
        let m = sw_eq(w, b',')
            | sw_eq(w, b'"')
            | sw_eq(w, b'\\')
            | sw_lt20(w)
            | sw_eq(w, 0xED);
        if m != 0 {
            return i + sw_first(m);
        }
        i += 8;
    }
    while i < n {
        let c = t[i];
        if c == b',' || c == b'"' || c == b'\\' || c < 0x20 || c == 0xED {
            return i;
        }
        i += 1;
    }
    n
}

fn find_event_quoted(t: &[u8], mut i: usize) -> usize {
    let n = t.len();
    while i + 8 <= n {
        let w = sw_word(t, i);
        let m = sw_eq(w, b'"') | sw_eq(w, b'\\') | sw_lt20(w) | sw_eq(w, 0xED);
        if m != 0 {
            return i + sw_first(m);
        }
        i += 8;
    }
    while i < n {
        let c = t[i];
        if c == b'"' || c == b'\\' || c < 0x20 || c == 0xED {
            return i;
        }
        i += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// JSON emission helpers (byte-identical to JSON.stringify of the value).
// ---------------------------------------------------------------------------

const HEX: &[u8; 16] = b"0123456789abcdef";

#[inline]
fn push_u32(out: &mut Vec<u8>, mut v: u32) {
    let mut buf = [0u8; 10];
    let mut k = 10;
    loop {
        k -= 1;
        buf[k] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[k..]);
}

/// Appends the UTF-8 (for a surrogate code unit: the WTF-8) encoding of `cp`.
#[inline]
fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.push(0xC0 | (cp >> 6) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else if cp < 0x10000 {
        out.push(0xE0 | (cp >> 12) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else {
        out.push(0xF0 | (cp >> 18) as u8);
        out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    }
}

/// Whether the 3 bytes at `i` are a WTF-8 lone-surrogate sequence (ED A0..BF 80..BF).
#[inline]
fn is_ed_surrogate(t: &[u8], i: usize) -> bool {
    i + 2 < t.len()
        && t[i] == 0xED
        && (0xA0..=0xBF).contains(&t[i + 1])
        && (0x80..=0xBF).contains(&t[i + 2])
}

/// Whether the 3 bytes at `i` are a WTF-8 low-surrogate sequence (ED B0..BF 80..BF).
#[inline]
fn is_ed_low(t: &[u8], i: usize) -> bool {
    i + 2 < t.len()
        && t[i] == 0xED
        && (0xB0..=0xBF).contains(&t[i + 1])
        && (0x80..=0xBF).contains(&t[i + 2])
}

/// The code unit of the surrogate sequence at `i` (caller checked it is one).
#[inline]
fn decode_ed(t: &[u8], i: usize) -> u32 {
    0xD000u32 | (((t[i + 1] & 0x3F) as u32) << 6) | (t[i + 2] & 0x3F) as u32
}

/// Emits `s` as a JSON string literal, escaping exactly as JSON.stringify does: the
/// short forms for quote, backslash and \b \f \n \r \t, \u00xx (lowercase hex) for
/// the other C0 controls, \udXXX for a lone surrogate's WTF-8 sequence — or the raw
/// astral character when a high surrogate stands directly before a low one — and
/// every other byte raw. Used for the cut fields of a long_row error, whose values
/// are materialized; the hot path emits strings with the same rules inline.
fn escape_into(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    let n = s.len();
    let mut seg = 0usize;
    let mut i = 0usize;
    while i < n {
        let j = find_esc(s, i);
        if j >= n {
            break;
        }
        match s[j] {
            b'"' => {
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\\"");
                i = j + 1;
            }
            b'\\' => {
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\\\");
                i = j + 1;
            }
            0x08 => {
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\b");
                i = j + 1;
            }
            0x09 => {
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\t");
                i = j + 1;
            }
            0x0A => {
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\n");
                i = j + 1;
            }
            0x0C => {
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\f");
                i = j + 1;
            }
            0x0D => {
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\r");
                i = j + 1;
            }
            0xED => {
                if is_ed_surrogate(s, j) {
                    let cu = decode_ed(s, j);
                    // A high surrogate directly followed by a low surrogate is one
                    // astral character to JSON.stringify: emit it raw, as it does.
                    if cu < 0xDC00 && is_ed_low(s, j + 3) {
                        let lo = decode_ed(s, j + 3);
                        let cp = 0x10000 + ((cu - 0xD800) << 10) + (lo - 0xDC00);
                        out.extend_from_slice(&s[seg..j]);
                        push_utf8(out, cp);
                        i = j + 6;
                        seg = i;
                        continue;
                    }
                    out.extend_from_slice(&s[seg..j]);
                    out.extend_from_slice(b"\\ud");
                    out.push(HEX[((cu >> 8) & 0xF) as usize]);
                    out.push(HEX[((cu >> 4) & 0xF) as usize]);
                    out.push(HEX[(cu & 0xF) as usize]);
                    i = j + 3;
                    seg = i;
                    continue;
                }
                // A real U+D000..D7FF character: not an event, stays in the span.
                i = j + 1;
                continue;
            }
            c => {
                // The remaining C0 controls.
                out.extend_from_slice(&s[seg..j]);
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(c >> 4) as usize]);
                out.push(HEX[(c & 0xF) as usize]);
                i = j + 1;
            }
        }
        seg = i;
    }
    out.extend_from_slice(&s[seg..n]);
    out.push(b'"');
}

// ---------------------------------------------------------------------------
// The CSV transducer proper.
// ---------------------------------------------------------------------------

const ENDED_END: u8 = 0;
const ENDED_COMMA: u8 = 1;
const ENDED_BREAK: u8 = 2;

const ERR_STRAY_CR: u8 = 0;
const ERR_UNTERMINATED: u8 = 1;
const ERR_QUOTE_JUNK: u8 = 2;
const ERR_SHORT_ROW: u8 = 3;
const ERR_LONG_ROW: u8 = 4;
const ERR_NAMES: [&str; 5] = [
    "stray_cr",
    "unterminated_quote",
    "quote_junk",
    "short_row",
    "long_row",
];

struct Err {
    code: u8,
    line: u32,
    row: Option<u32>,
    fields: u32,
    extra: Vec<Vec<u8>>,
}

/// Reusable scratch buffers, kept across cases so a whole envelope costs no fresh
/// allocations per case.
#[derive(Default)]
struct Bu {
    first: Vec<u8>,
    errors: Vec<Err>,
    field: Vec<u8>,
}

/// The record destination: the first record goes to the capture buffer
/// `first` (whether its bytes are the columns array or row 0 is known only
/// once the record is complete); every later record streams straight into
/// the answer buffer `out`.
macro_rules! dest {
    ($self:ident) => {
        (if $self.dest_first {
            &mut $self.bu.first
        } else {
            &mut *$self.out
        })
    };
}

struct Tr<'a> {
    t: &'a [u8],
    header: bool,
    bu: &'a mut Bu,
    pos: usize,
    line: u32,
    width: i32,
    have_cols: bool,
    rows_count: u32,
    rec_line: u32,
    rec_row: Option<u32>,
    dest_first: bool,
    first_done: bool,
    out: &'a mut Vec<u8>,
    field_cp: usize,
    /// A high surrogate just read whose emission is held back: if the field's
    /// next value character is a low surrogate, the two are one astral character
    /// to JSON.stringify and are emitted raw together; anything else first
    /// flushes the held surrogate as its \udXXX escape. 0 when none is held.
    pending_high: u32,
    /// Raw write cursor over the active destination buffer's spare capacity.
    /// Per-write Vec calls cost more than everything else at this scale, so the
    /// transducer writes through these pointers instead; `o_sync` publishes the
    /// written length back to the Vec before anything reads or grows it.
    obase: *mut u8,
    ocur: *mut u8,
    oend: *mut u8,
}

impl<'a> Tr<'a> {
    fn new(t: &'a [u8], header: bool, bu: &'a mut Bu, out: &'a mut Vec<u8>) -> Self {
        bu.first.clear();
        bu.errors.clear();
        bu.field.clear();
        out.reserve(t.len() + t.len() / 2);
        Tr {
            t,
            header,
            bu,
            out,
            pos: 0,
            line: 1,
            width: -1,
            have_cols: false,
            rows_count: 0,
            rec_line: 1,
            rec_row: None,
            dest_first: true,
            first_done: false,
            field_cp: 0,
            pending_high: 0,
            obase: std::ptr::null_mut(),
            ocur: std::ptr::null_mut(),
            oend: std::ptr::null_mut(),
        }
    }

    /// Points the write cursor at the active destination buffer.
    fn o_begin(&mut self) {
        let d = dest!(self);
        let len = d.len();
        let cap = d.capacity();
        let base = d.as_mut_ptr();
        self.obase = base;
        unsafe {
            self.ocur = base.add(len);
            self.oend = base.add(cap);
        }
    }

    /// Publishes the cursor's written length to the active destination Vec.
    fn o_sync(&mut self) {
        if self.obase.is_null() {
            return;
        }
        let n = (self.ocur as usize).wrapping_sub(self.obase as usize);
        let d = dest!(self);
        unsafe { d.set_len(n) };
    }

    /// How much has been written into the active destination so far.
    #[inline]
    fn o_len(&self) -> usize {
        (self.ocur as usize).wrapping_sub(self.obase as usize)
    }

    /// Ensures at least `n` more bytes fit under the cursor.
    #[inline]
    fn o_grow(&mut self, n: usize) {
        self.o_sync();
        dest!(self).reserve(n);
        self.o_begin();
    }

    /// Truncates the active destination back to `len` written bytes (a
    /// checkpoint taken with `o_len`): the unterminated-quote and
    /// discarded-record rollbacks.
    #[inline]
    fn o_truncate(&mut self, len: usize) {
        unsafe { self.ocur = self.obase.add(len) };
    }

    /// Writes one byte through the cursor.
    #[inline]
    fn o_byte(&mut self, b: u8) {
        if self.ocur == self.oend {
            self.o_grow(64);
        }
        unsafe {
            *self.ocur = b;
            self.ocur = self.ocur.add(1);
        }
    }

    /// Writes a whole slice through the cursor.
    #[inline]
    fn o_bytes(&mut self, s: &[u8]) {
        if (self.oend as usize).wrapping_sub(self.ocur as usize) < s.len() {
            self.o_grow(s.len());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), self.ocur, s.len());
            self.ocur = self.ocur.add(s.len());
        }
    }

    #[inline]
    fn at(&self, i: usize) -> u8 {
        if i < self.t.len() {
            self.t[i]
        } else {
            0
        }
    }

    #[inline]
    fn err(&mut self, code: u8) {
        self.bu.errors.push(Err {
            code,
            line: self.rec_line,
            row: self.rec_row,
            fields: 0,
            extra: Vec::new(),
        });
    }

    /// Emits the held high surrogate's escape, if one is held, and returns how
    /// many value bytes that accounted (3 or 0). Called before anything that
    /// emits a different value character, and at every field end.
    #[inline]
    #[inline]
    fn flush_pending(&mut self) -> usize {
        if self.pending_high != 0 {
            let cu = self.pending_high;
            self.pending_high = 0;
            self.push_u_escape(cu);
            3
        } else {
            0
        }
    }

    /// Copies the raw span t[from..to] into the output, when it is not empty,
    /// first resolving a held high surrogate (a span's first character is a
    /// plain character, never a low surrogate: spans hold no 0xED bytes).
    /// Returns the value bytes accounted.
    #[inline]
    fn flush_span(&mut self, from: usize, to: usize) -> usize {
        if to > from {
            let d = self.flush_pending();
            let t = self.t;
            self.o_bytes(&t[from..to]);
            d + (to - from)
        } else {
            0
        }
    }

    /// Writes the JSON escape of one value byte (quote, backslash, or a C0 control)
    /// into the output.
    #[inline]
    fn push_escape(&mut self, c: u8) {
        match c {
            b'"' => self.o_bytes(b"\\\""),
            b'\\' => self.o_bytes(b"\\\\"),
            0x08 => self.o_bytes(b"\\b"),
            0x09 => self.o_bytes(b"\\t"),
            0x0A => self.o_bytes(b"\\n"),
            0x0C => self.o_bytes(b"\\f"),
            0x0D => self.o_bytes(b"\\r"),
            _ => {
                self.o_bytes(b"\\u00");
                self.o_byte(HEX[(c >> 4) as usize]);
                self.o_byte(HEX[(c & 0xF) as usize]);
            }
        }
    }

    /// Writes the \udXXX escape of a lone surrogate code unit into the output.
    #[inline]
    fn push_u_escape(&mut self, cu: u32) {
        self.o_bytes(b"\\ud");
        self.o_byte(HEX[((cu >> 8) & 0xF) as usize]);
        self.o_byte(HEX[((cu >> 4) & 0xF) as usize]);
        self.o_byte(HEX[(cu & 0xF) as usize]);
    }

    /// Handles an escape event at `j` in the direct-emitting readers: a quote, a
    /// backslash, a C0 control other than LF/CR, or a lone surrogate's WTF-8
    /// sequence (checked by the caller). Emits the byte's JSON form. A high
    /// surrogate is not emitted at once but held (see `pending_high`): a low
    /// surrogate directly after it in the text completes the pair on the spot;
    /// one reached later — across dropped stray CRs, or across a closing quote
    /// into the junk tail — completes it when it arrives. Returns (text bytes
    /// consumed, value bytes accounted).
    /// Writes an astral code point (the only kind a surrogate pair makes) as
    /// its 4 raw UTF-8 bytes through the cursor.
    #[inline]
    fn o_utf8(&mut self, cp: u32) {
        let tmp = [
            0xF0 | ((cp >> 18) as u8),
            0x80 | (((cp >> 12) & 0x3F) as u8),
            0x80 | (((cp >> 6) & 0x3F) as u8),
            0x80 | ((cp & 0x3F) as u8),
        ];
        self.o_bytes(&tmp);
    }

    fn emit_escape_event(&mut self, j: usize) -> (usize, usize) {
        let c = self.t[j];
        match c {
            b'"' => {
                let d = self.flush_pending();
                self.o_bytes(b"\\\"");
                (1, d + 1)
            }
            b'\\' => {
                let d = self.flush_pending();
                self.o_bytes(b"\\\\");
                (1, d + 1)
            }
            0xED => {
                let cu = decode_ed(self.t, j);
                if cu < 0xDC00 {
                    // A high surrogate. Whatever was held before it is not its
                    // partner (a high surrogate is no low surrogate), so flush
                    // that first.
                    let d = self.flush_pending();
                    if is_ed_low(self.t, j + 3) {
                        let lo = decode_ed(self.t, j + 3);
                        let cp = 0x10000 + ((cu - 0xD800) << 10) + (lo - 0xDC00);
                        self.o_utf8(cp);
                        return (6, d + 6);
                    }
                    self.pending_high = cu;
                    (3, d)
                } else {
                    // A low surrogate: the partner of the held high surrogate,
                    // if one is held; a lone escape otherwise.
                    if self.pending_high != 0 {
                        let hi = self.pending_high;
                        self.pending_high = 0;
                        let cp = 0x10000 + ((hi - 0xD800) << 10) + (cu - 0xDC00);
                        self.o_utf8(cp);
                        return (3, 6);
                    }
                    self.push_u_escape(cu);
                    (3, 3)
                }
            }
            _ => {
                let d = self.flush_pending();
                self.push_escape(c);
                (1, d + 1)
            }
        }
    }

    /// The direct-emitting unquoted reader's loop: writes the field's content as
    /// JSON into the output (the caller owns the surrounding quotes), from the
    /// current position. Returns how the field ended and how many value bytes it
    /// appended.
    fn plain_loop(&mut self) -> (u8, usize) {
        let mut vlen = 0usize;
        let mut seg = self.pos;
        loop {
            let j = find_event_plain(self.t, self.pos);
            if j >= self.t.len() {
                vlen += self.flush_span(seg, self.t.len());
                vlen += self.flush_pending();
                self.pos = self.t.len();
                return (ENDED_END, vlen);
            }
            match self.t[j] {
                b',' => {
                    vlen += self.flush_span(seg, j);
                    vlen += self.flush_pending();
                    self.pos = j + 1;
                    return (ENDED_COMMA, vlen);
                }
                b'\n' => {
                    vlen += self.flush_span(seg, j);
                    vlen += self.flush_pending();
                    self.pos = j + 1;
                    self.line += 1;
                    return (ENDED_BREAK, vlen);
                }
                b'\r' => {
                    if self.at(j + 1) == b'\n' {
                        vlen += self.flush_span(seg, j);
                        vlen += self.flush_pending();
                        self.pos = j + 2;
                        self.line += 1;
                        return (ENDED_BREAK, vlen);
                    }
                    // A stray CR is dropped: the span before it is kept, the CR
                    // itself contributes nothing — and it is no value character,
                    // so a held high surrogate stays held across it.
                    vlen += self.flush_span(seg, j);
                    self.err(ERR_STRAY_CR);
                    self.pos = j + 1;
                    seg = self.pos;
                }
                0xED if !is_ed_surrogate(self.t, j) => {
                    // A real U+D000..D7FF character: part of the raw span.
                    self.pos = j + 1;
                }
                _ => {
                    vlen += self.flush_span(seg, j);
                    let (consumed, vd) = self.emit_escape_event(j);
                    vlen += vd;
                    self.pos = j + consumed;
                    seg = self.pos;
                }
            }
        }
    }

    /// Reads an unquoted field, emitting its JSON. `head_quote` is the re-read of a
    /// never-closed quote, whose value starts with a literal quote character.
    /// Reads one field that fits the record's width, emitting its JSON string
    /// through the write cursor. Returns how the field ended and how many
    /// value bytes it holds.
    fn field_direct(&mut self, quoted: bool) -> (u8, usize) {
        if !quoted {
            self.o_byte(b'"');
            let (ended, added) = self.plain_loop();
            self.o_byte(b'"');
            return (ended, added);
        }
        self.field_quoted()
    }

    /// The quoted-field path of `field_direct`, kept in its own function so the
    /// plain path's code stays small (the runtime's fast compiler tier is
    /// especially sensitive to function size).
    fn field_quoted(&mut self) -> (u8, usize) {
        // `field_cp` is the output checkpoint from before the opening quote
        // was written.
        let cp = self.field_cp;
        let open = self.pos;
        let open_line = self.line;
        self.o_byte(b'"');
        self.pos += 1;
        let mut vlen = 0usize;
        let mut scan = self.pos;
        let mut seg = self.pos;
        loop {
            if scan >= self.t.len() {
                // The quote never closes: it is a literal character, the line
                // count goes back to what it was at the opening quote, and the
                // text after the quote is read again as unquoted.
                self.line = open_line;
                self.err(ERR_UNTERMINATED);
                self.pending_high = 0;
                self.o_truncate(cp);
                self.pos = open + 1;
                self.o_byte(b'"');
                self.o_bytes(b"\\\"");
                let (ended, added) = self.plain_loop();
                self.o_byte(b'"');
                return (ended, 1 + added);
            }
            let j = find_event_quoted(self.t, scan);
            if j >= self.t.len() {
                scan = j;
                continue;
            }
            match self.t[j] {
                b'"' => {
                    if self.at(j + 1) == b'"' {
                        // A doubled quote inside the field is one literal quote.
                        vlen += self.flush_span(seg, j);
                        vlen += self.flush_pending();
                        self.o_bytes(b"\\\"");
                        vlen += 1;
                        scan = j + 2;
                        seg = scan;
                        continue;
                    }
                    // The closing quote. It is no value character, so a held
                    // high surrogate stays held into whatever follows: the junk
                    // tail can still complete its pair.
                    vlen += self.flush_span(seg, j);
                    let p = j + 1;
                    self.pos = p;
                    if p >= self.t.len() {
                        vlen += self.flush_pending();
                        self.o_byte(b'"');
                        return (ENDED_END, vlen);
                    }
                    match self.t[p] {
                        b',' => {
                            self.pos = p + 1;
                            vlen += self.flush_pending();
                            self.o_byte(b'"');
                            return (ENDED_COMMA, vlen);
                        }
                        b'\n' => {
                            self.pos = p + 1;
                            self.line += 1;
                            vlen += self.flush_pending();
                            self.o_byte(b'"');
                            return (ENDED_BREAK, vlen);
                        }
                        b'\r' if self.at(p + 1) == b'\n' => {
                            self.pos = p + 2;
                            self.line += 1;
                            vlen += self.flush_pending();
                            self.o_byte(b'"');
                            return (ENDED_BREAK, vlen);
                        }
                        _ => {
                            // Anything else after the closing quote is junk,
                            // appended as it stands up to the next comma or
                            // line break; reported when the field ends, if it
                            // appended anything. A high surrogate held from
                            // the quoted content resolves during the tail, but
                            // its 3 bytes are the content's, not the junk's.
                            let held_bytes = if self.pending_high != 0 { 3 } else { 0 };
                            let (ended, added) = self.plain_loop();
                            vlen += added;
                            if added > held_bytes {
                                self.err(ERR_QUOTE_JUNK);
                            }
                            self.o_byte(b'"');
                            return (ended, vlen);
                        }
                    }
                }
                b'\r' => {
                    // A CRLF or a lone CR inside quotes reads as a single LF.
                    vlen += self.flush_span(seg, j);
                    vlen += self.flush_pending();
                    self.push_escape(0x0A);
                    vlen += 1;
                    self.line += 1;
                    scan = j + if self.at(j + 1) == b'\n' { 2 } else { 1 };
                    seg = scan;
                }
                b'\n' => {
                    // An LF inside quotes is field text, and one more line.
                    vlen += self.flush_span(seg, j);
                    vlen += self.flush_pending();
                    self.push_escape(0x0A);
                    vlen += 1;
                    self.line += 1;
                    scan = j + 1;
                    seg = scan;
                }
                0xED if !is_ed_surrogate(self.t, j) => {
                    scan = j + 1;
                }
                _ => {
                    vlen += self.flush_span(seg, j);
                    let (consumed, vd) = self.emit_escape_event(j);
                    vlen += vd;
                    scan = j + consumed;
                    seg = scan;
                }
            }
        }
    }

    fn read_plain_value_tail(&mut self) -> (u8, usize) {
        let base = self.bu.field.len();
        loop {
            let j = find_plain(self.t, self.pos);
            self.bu.field.extend_from_slice(&self.t[self.pos..j]);
            self.pos = j;
            if j >= self.t.len() {
                return (ENDED_END, self.bu.field.len() - base);
            }
            match self.t[j] {
                b',' => {
                    self.pos = j + 1;
                    return (ENDED_COMMA, self.bu.field.len() - base);
                }
                b'\n' => {
                    self.pos = j + 1;
                    self.line += 1;
                    return (ENDED_BREAK, self.bu.field.len() - base);
                }
                _ => {
                    if self.at(j + 1) == b'\n' {
                        self.pos = j + 2;
                        self.line += 1;
                        return (ENDED_BREAK, self.bu.field.len() - base);
                    }
                    self.err(ERR_STRAY_CR);
                    self.pos = j + 1;
                }
            }
        }
    }

    /// Reads an unquoted field into the field buffer, on top of `head` (""
    /// normally, '"' when a never-closed quote is re-read).
    fn read_plain_value(&mut self, head: &[u8]) -> u8 {
        self.bu.field.clear();
        self.bu.field.extend_from_slice(head);
        self.read_plain_value_tail().0
    }

    /// Reads a field whose opening quote is at the current position into the
    /// field buffer.
    fn read_quoted_value(&mut self) -> u8 {
        let open = self.pos;
        let open_line = self.line;
        self.bu.field.clear();
        let mut scan = open + 1;
        let mut seg = scan;
        loop {
            if scan >= self.t.len() {
                self.pos = open + 1;
                self.line = open_line;
                self.err(ERR_UNTERMINATED);
                return self.read_plain_value(b"\"");
            }
            let j = find_quoted(self.t, scan);
            if j >= self.t.len() {
                scan = j;
                continue;
            }
            match self.t[j] {
                b'\n' => {
                    self.line += 1;
                    scan = j + 1;
                }
                b'\r' => {
                    self.bu.field.extend_from_slice(&self.t[seg..j]);
                    self.bu.field.push(b'\n');
                    self.line += 1;
                    scan = j + if self.at(j + 1) == b'\n' { 2 } else { 1 };
                    seg = scan;
                }
                _ => {
                    if self.at(j + 1) == b'"' {
                        self.bu.field.extend_from_slice(&self.t[seg..j]);
                        self.bu.field.push(b'"');
                        scan = j + 2;
                        seg = scan;
                        continue;
                    }
                    self.bu.field.extend_from_slice(&self.t[seg..j]);
                    let p = j + 1;
                    self.pos = p;
                    if p >= self.t.len() {
                        return ENDED_END;
                    }
                    match self.t[p] {
                        b',' => {
                            self.pos = p + 1;
                            return ENDED_COMMA;
                        }
                        b'\n' => {
                            self.pos = p + 1;
                            self.line += 1;
                            return ENDED_BREAK;
                        }
                        b'\r' if self.at(p + 1) == b'\n' => {
                            self.pos = p + 2;
                            self.line += 1;
                            return ENDED_BREAK;
                        }
                        _ => {
                            let (ended, added) = self.read_plain_value_tail();
                            if added > 0 {
                                self.err(ERR_QUOTE_JUNK);
                            }
                            return ended;
                        }
                    }
                }
            }
        }
    }

    fn parse_all(&mut self) {
        let len = self.t.len();
        // A leading U+FEFF is dropped: in WTF-8 bytes that is exactly EF BB BF at 0.
        if len >= 3 && self.t[0] == 0xEF && self.t[1] == 0xBB && self.t[2] == 0xBF {
            self.pos = 3;
        }
        self.o_begin();
        loop {
            // Lines with no characters are no records: skip bare LF and CRLF line
            // ends.
            while self.pos < len {
                let c = self.t[self.pos];
                if c == b'\n' {
                    self.pos += 1;
                    self.line += 1;
                } else if c == b'\r' && self.at(self.pos + 1) == b'\n' {
                    self.pos += 2;
                    self.line += 1;
                } else {
                    break;
                }
            }
            if self.pos >= len {
                break;
            }

            self.rec_line = self.line;
            self.rec_row = if self.header && !self.have_cols {
                None
            } else {
                Some(self.rows_count)
            };
            let want_first = !self.first_done;
            if want_first != self.dest_first {
                self.o_sync();
                self.dest_first = want_first;
                self.o_begin();
            }
            let rec_cp = self.o_len();
            if !self.dest_first && self.rows_count > 0 {
                self.o_byte(b',');
            }
            self.o_byte(b'[');

            let mut extras: Vec<Vec<u8>> = Vec::new();
            let mut field_count = 0usize;
            let mut last_quoted = false;
            let mut first_vlen = 0usize;
            let ended = loop {
                let quoted = self.at(self.pos) == b'"';
                last_quoted = quoted;
                let (ended, vlen) =
                    if self.width >= 0 && field_count >= self.width as usize {
                        // A field past the first record's width: its value goes
                        // to the record's long_row error, so it takes the
                        // value-building reader.
                        let e = if quoted {
                            self.read_quoted_value()
                        } else {
                            self.read_plain_value(b"")
                        };
                        extras.push(std::mem::take(&mut self.bu.field));
                        (e, 0)
                    } else {
                        if field_count > 0 {
                            self.o_byte(b',');
                        }
                        self.field_cp = self.o_len();
                        self.field_direct(quoted)
                    };
                if field_count == 0 {
                    first_vlen = vlen;
                }
                field_count += 1;
                if ended != ENDED_COMMA {
                    break ended;
                }
            };
            let _ = ended;
            self.pending_high = 0;

            // A line that held only dropped stray CRs is left with no
            // characters: no record (the stray_cr errors it produced stay).
            if field_count == 1 && first_vlen == 0 && !last_quoted {
                self.o_truncate(rec_cp);
                continue;
            }

            if self.width < 0 {
                self.width = field_count as i32;
            }
            let w = self.width as usize;

            if self.dest_first {
                // The first record is complete in the capture buffer: close
                // it, publish the write cursor, then emit the answer preamble
                // into the output — the record itself is the columns array
                // when the header flag is set, row 0 otherwise — and stream
                // every later record straight into the output.
                self.o_byte(b']');
                self.o_sync();
                if self.header {
                    self.have_cols = true;
                }
                self.out.extend_from_slice(b"{\"columns\":");
                if self.header {
                    self.out.extend_from_slice(&self.bu.first);
                } else {
                    self.out.extend_from_slice(b"null");
                }
                self.out.extend_from_slice(b",\"rows\":[");
                if !self.header {
                    self.out.extend_from_slice(&self.bu.first);
                    self.rows_count += 1;
                }
                self.bu.first.clear();
                self.first_done = true;
                self.dest_first = false;
                self.o_begin();
                continue;
            }

            if field_count < w {
                self.bu.errors.push(Err {
                    code: ERR_SHORT_ROW,
                    line: self.rec_line,
                    row: self.rec_row,
                    fields: field_count as u32,
                    extra: Vec::new(),
                });
                for _ in field_count..w {
                    self.o_bytes(b",\"\"");
                }
            } else if field_count > w {
                self.bu.errors.push(Err {
                    code: ERR_LONG_ROW,
                    line: self.rec_line,
                    row: self.rec_row,
                    fields: 0,
                    extra: extras,
                });
            }
            self.o_byte(b']');
            self.rows_count += 1;
        }
        self.o_sync();
        if !self.first_done {
            // No record at all: the preamble still has to be written.
            self.out.extend_from_slice(b"{\"columns\":");
            self.out
                .extend_from_slice(if self.header { b"[]" as &[u8] } else { b"null" });
            self.out.extend_from_slice(b",\"rows\":[");
            self.first_done = true;
        }
    }

    fn run(&mut self) {
        self.parse_all();
        // The errors tail is written with plain Vec writes (it is cold, and
        // the long_row extras need Vec-level emission anyway).
        self.out.extend_from_slice(b"],\"errors\":[");
        for (i, e) in self.bu.errors.iter().enumerate() {
            if i > 0 {
                self.out.push(b',');
            }
            self.out.extend_from_slice(b"{\"code\":\"");
            self.out.extend_from_slice(ERR_NAMES[e.code as usize].as_bytes());
            self.out.extend_from_slice(b"\",\"line\":");
            push_u32(self.out, e.line);
            self.out.extend_from_slice(b",\"row\":");
            match e.row {
                None => self.out.extend_from_slice(b"null"),
                Some(r) => push_u32(self.out, r),
            }
            match e.code {
                ERR_SHORT_ROW => {
                    self.out.extend_from_slice(b",\"fields\":");
                    push_u32(self.out, e.fields);
                }
                ERR_LONG_ROW => {
                    self.out.extend_from_slice(b",\"extra\":[");
                    for (k, f) in e.extra.iter().enumerate() {
                        if k > 0 {
                            self.out.push(b',');
                        }
                        escape_into(self.out, f);
                    }
                    self.out.push(b']');
                }
                _ => {}
            }
            self.out.push(b'}');
        }
        self.out.extend_from_slice(b"]}");
    }
}

// ---------------------------------------------------------------------------
// The cases.json envelope: a forgiving scanner that finds each case's "input"
// object (any key order, extra keys tolerated), unescapes its "text" string to
// WTF-8 bytes, and reads "header" as the JSON literal true.
// ---------------------------------------------------------------------------

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    #[inline]
    fn ws(&mut self) {
        while self.i < self.b.len()
            && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.i += 1;
        }
    }

    #[inline]
    fn peek(&self) -> u8 {
        if self.i < self.b.len() {
            self.b[self.i]
        } else {
            0
        }
    }

    /// Consumes the string at the cursor (which must sit on its opening quote)
    /// and returns the (start, end) of its raw content plus whether it held an
    /// escape.
    fn string_span(&mut self) -> (usize, usize, bool) {
        if self.peek() != b'"' {
            return (self.i, self.i, false);
        }
        let start = self.i + 1;
        let mut j = start;
        let mut escaped = false;
        loop {
            let k = find_unesc(self.b, j);
            if k >= self.b.len() {
                self.i = self.b.len();
                return (start, self.b.len(), escaped);
            }
            if self.b[k] == b'"' {
                self.i = k + 1;
                return (start, k, escaped);
            }
            escaped = true;
            // Skip the escaped character; for \u the four hex digits that
            // follow hold neither a quote nor a backslash, so scanning on from
            // k + 2 is safe.
            j = k + 2;
        }
    }

    /// Unescapes the string at the cursor (on its opening quote) into `out` as
    /// WTF-8 bytes, consuming through the closing quote.
    fn unescape_into(&mut self, out: &mut Vec<u8>) {
        self.i += 1; // the opening quote
        loop {
            let j = find_unesc(self.b, self.i);
            out.extend_from_slice(&self.b[self.i..j]);
            self.i = j;
            if j >= self.b.len() {
                return;
            }
            if self.b[j] == b'"' {
                self.i = j + 1;
                return;
            }
            let e = if j + 1 < self.b.len() { self.b[j + 1] } else { 0 };
            match e {
                b'"' => {
                    out.push(b'"');
                    self.i = j + 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    self.i = j + 2;
                }
                b'/' => {
                    out.push(b'/');
                    self.i = j + 2;
                }
                b'b' => {
                    out.push(0x08);
                    self.i = j + 2;
                }
                b'f' => {
                    out.push(0x0C);
                    self.i = j + 2;
                }
                b'n' => {
                    out.push(b'\n');
                    self.i = j + 2;
                }
                b'r' => {
                    // "\r\n" is how every CRLF line ending arrives: take both
                    // escapes in one step.
                    if j + 3 < self.b.len() && self.b[j + 2] == b'\\' && self.b[j + 3] == b'n' {
                        out.extend_from_slice(b"\r\n");
                        self.i = j + 4;
                    } else {
                        out.push(b'\r');
                        self.i = j + 2;
                    }
                }
                b't' => {
                    out.push(0x09);
                    self.i = j + 2;
                }
                b'u' => {
                    let cu = hex4(self.b, j + 2);
                    let adv = j + 6;
                    if (0xD800..0xDC00).contains(&cu) {
                        // A high surrogate: a following \uDC00..\uDFFF completes
                        // the pair (one astral character); anything else leaves
                        // it lone, and its 3-byte encoding below is exactly
                        // WTF-8 for it.
                        if adv + 6 <= self.b.len()
                            && self.b[adv] == b'\\'
                            && self.b[adv + 1] == b'u'
                        {
                            let lo = hex4(self.b, adv + 2);
                            if (0xDC00..0xE000).contains(&lo) {
                                let cp = 0x10000
                                    + (((cu as u32) - 0xD800) << 10)
                                    + ((lo as u32) - 0xDC00);
                                push_utf8(out, cp);
                                self.i = adv + 6;
                                continue;
                            }
                        }
                        push_utf8(out, cu as u32);
                        self.i = adv;
                    } else {
                        // A BMP character, or a lone low surrogate (whose
                        // 3-byte encoding is again exactly WTF-8 for it).
                        push_utf8(out, cu as u32);
                        self.i = adv;
                    }
                }
                _ => {
                    out.push(e);
                    self.i = j + 2;
                }
            }
        }
    }

    fn skip_value(&mut self) {
        self.ws();
        match self.peek() {
            b'"' => {
                self.string_span();
            }
            b'{' => {
                self.i += 1;
                loop {
                    self.ws();
                    if self.peek() == b'}' {
                        self.i += 1;
                        return;
                    }
                    if self.peek() == 0 {
                        return;
                    }
                    self.string_span();
                    self.ws();
                    if self.peek() == b':' {
                        self.i += 1;
                    }
                    self.skip_value();
                    self.ws();
                    if self.peek() == b',' {
                        self.i += 1;
                        continue;
                    }
                    if self.peek() == b'}' {
                        self.i += 1;
                    }
                    return;
                }
            }
            b'[' => {
                self.i += 1;
                loop {
                    self.ws();
                    if self.peek() == b']' {
                        self.i += 1;
                        return;
                    }
                    if self.peek() == 0 {
                        return;
                    }
                    self.skip_value();
                    self.ws();
                    if self.peek() == b',' {
                        self.i += 1;
                        continue;
                    }
                    if self.peek() == b']' {
                        self.i += 1;
                    }
                    return;
                }
            }
            _ => {
                while self.i < self.b.len()
                    && !matches!(
                        self.b[self.i],
                        b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'
                    )
                {
                    self.i += 1;
                }
            }
        }
    }

    /// Whether the key whose raw span is (ks, ke) equals `target`.
    fn key_is(&self, ks: usize, ke: usize, escaped: bool, target: &[u8]) -> bool {
        if !escaped {
            return &self.b[ks..ke] == target;
        }
        // Keys with escapes are vanishingly rare; unescape and compare.
        let mut tmp = Cur { b: self.b, i: ks - 1 };
        let mut v = Vec::new();
        tmp.unescape_into(&mut v);
        v == target
    }
}

fn hex4(b: &[u8], i: usize) -> u16 {
    let mut v: u16 = 0;
    for k in 0..4 {
        let c = if i + k < b.len() { b[i + k] } else { 0 };
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => 0,
        };
        v = (v << 4) | d as u16;
    }
    v
}

fn parse_input(cur: &mut Cur, text: &mut Vec<u8>, header: &mut bool) {
    cur.ws();
    if cur.peek() != b'{' {
        cur.skip_value();
        return;
    }
    cur.i += 1;
    loop {
        cur.ws();
        if cur.peek() == b'}' {
            cur.i += 1;
            return;
        }
        if cur.peek() == 0 {
            return;
        }
        let (ks, ke, esc) = cur.string_span();
        cur.ws();
        if cur.peek() == b':' {
            cur.i += 1;
        }
        cur.ws();
        if cur.key_is(ks, ke, esc, b"text") {
            if cur.peek() == b'"' {
                cur.unescape_into(text);
            } else {
                cur.skip_value();
            }
        } else if cur.key_is(ks, ke, esc, b"header") {
            if cur.peek() == b't' {
                *header = true;
                cur.i += 4;
            } else if cur.peek() == b'f' {
                *header = false;
                cur.i += 5;
            } else {
                cur.skip_value();
            }
        } else {
            cur.skip_value();
        }
        cur.ws();
        if cur.peek() == b',' {
            cur.i += 1;
            continue;
        }
        if cur.peek() == b'}' {
            cur.i += 1;
        }
        return;
    }
}

fn parse_case(cur: &mut Cur, text: &mut Vec<u8>, header: &mut bool) {
    cur.ws();
    if cur.peek() != b'{' {
        cur.skip_value();
        return;
    }
    cur.i += 1;
    loop {
        cur.ws();
        if cur.peek() == b'}' {
            cur.i += 1;
            return;
        }
        if cur.peek() == 0 {
            return;
        }
        let (ks, ke, esc) = cur.string_span();
        cur.ws();
        if cur.peek() == b':' {
            cur.i += 1;
        }
        cur.ws();
        if cur.key_is(ks, ke, esc, b"input") {
            parse_input(cur, text, header);
        } else {
            cur.skip_value();
        }
        cur.ws();
        if cur.peek() == b',' {
            cur.i += 1;
            continue;
        }
        if cur.peek() == b'}' {
            cur.i += 1;
        }
        return;
    }
}

/// DEV ONLY: envelope walk + unescape, no CSV parse. Returns total text bytes.
#[no_mangle]
fn transduce_all(input: &[u8], out: &mut Vec<u8>) {
    let mut cur = Cur { b: input, i: 0 };
    let mut bu = Bu::default();
    let mut text: Vec<u8> = Vec::new();
    out.push(b'[');
    cur.ws();
    if cur.peek() == b'[' {
        cur.i += 1;
    }
    let mut count = 0usize;
    loop {
        cur.ws();
        if cur.peek() == b']' {
            cur.i += 1;
            break;
        }
        if cur.peek() == 0 {
            break;
        }
        if count > 0 {
            if cur.peek() == b',' {
                cur.i += 1;
            }
            out.push(b',');
        }
        text.clear();
        let mut header = false;
        parse_case(&mut cur, &mut text, &mut header);
        let mut tr = Tr::new(&text, header, &mut bu, &mut *out);
        tr.run();
        count += 1;
    }
    out.push(b']');
}

