// A fast reader for broken CSV text.
//
// run(input) answers one case's input {text, header} with {columns, rows, errors}, following the
// csv-reader 1.0.0 reading policy exactly:
//
//   - a U+FEFF at the very start of the text is dropped;
//   - a record ends at LF or CRLF, and a line left with no characters is no record;
//   - outside quotes, a CR not followed by LF is dropped and reported as stray_cr;
//   - a field that opens with a quote runs to the next quote that is not doubled: "" inside it is
//     one quote, and each CRLF or lone CR inside it reads as one LF; whatever follows the closing
//     quote, up to the next comma or line break, is appended as it stands (quote_junk, reported
//     when the field ends, if it appended anything); a quote that never closes is a literal
//     character and the text after it is read again as an unquoted field (unterminated_quote);
//   - a quote inside an unquoted field is a literal character; fields stay strings, never
//     trimmed or converted;
//   - columns is the first record when header is true, else null; rows holds the records after
//     it (every record when header is false), each padded with empty strings (short_row, with
//     fields: its own count) or cut (long_row, with extra: the cut fields) to the first record's
//     width;
//   - each error is {code, line, row}: line is the 1-based line the record starts on, counting
//     every LF, CRLF and lone CR inside quotes; row is the record's index in rows, null for the
//     header; errors come in the order met, a record's short_row or long_row when it ends.
//
// The reference program walks the text one character at a time and grows each field one
// character at a time. This reader instead scans ahead to the next character that can change
// anything (a comma, a quote, a CR or an LF) and copies each plain stretch of a field with a
// single slice, so clean text costs one scan and one slice per field instead of one string
// concatenation per character. The broken corners take the slower paths below, which mirror the
// policy step for step.

// How a field ended: at a comma, at a line break, or at the end of the text.
const AT_COMMA = 0;
const AT_BREAK = 1;
const AT_END = 2;

/** csv-reader 1.0.0's answer for one case's input {text, header}: {columns, rows, errors}. */
export function run(input) {
  const text = String(input.text);
  const header = input.header === true;
  const len = text.length;

  const errors = [];
  const rows = [];
  let columns = null;
  let width = -1;

  let pos = 0; // the next character to read
  let line = 1; // the line that character is on
  if (text.charCodeAt(0) === 0xfeff) pos = 1; // a leading byte order mark is dropped

  // The record being read: the line it starts on and the index in rows it takes (null: header).
  let recLine = 1;
  let recRow = null;

  // The field the readers below produced: its value and how it ended.
  let fValue = '';
  let fEnd = AT_END;

  function note(code) {
    errors.push({ code, line: recLine, row: recRow });
  }

  // Reads an unquoted field body starting at pos, on top of `head` (text that already belongs
  // to the field: '' normally, '"' when a never-closed quote is re-read, or a quoted field's
  // content when its junk tail is read). Sets fValue and fEnd, leaves pos just past the
  // terminator, and returns how many characters this call appended to the field.
  function readPlain(head) {
    let parts = null; // field pieces, allocated only when a stray CR splits the field
    let segFrom = pos; // start of the plain stretch being scanned
    let scan = pos;
    let appended = 0;

    // Closes the field: the last plain stretch runs from segFrom to scan, and the field's
    // terminator moves pos to nextPos.
    function finish(nextPos, how) {
      const tail = text.slice(segFrom, scan);
      appended += tail.length;
      if (parts === null) {
        fValue = head + tail;
      } else {
        parts.push(tail);
        fValue = parts.join('');
      }
      pos = nextPos;
      fEnd = how;
      return appended;
    }

    for (;;) {
      // Scan one plain stretch: everything up to the next comma, LF or CR is field text.
      while (scan < len) {
        const cc = text.charCodeAt(scan);
        if (cc === 44 || cc === 10 || cc === 13) break;
        scan += 1;
      }
      if (scan >= len) return finish(len, AT_END);
      const cc = text.charCodeAt(scan);
      if (cc === 44) return finish(scan + 1, AT_COMMA);
      if (cc === 10) {
        line += 1;
        return finish(scan + 1, AT_BREAK);
      }
      // A CR: followed by LF it ends the record; on its own it is dropped (stray_cr) and the
      // field goes on without it, so the stretch before it is kept as a separate piece.
      if (text.charCodeAt(scan + 1) === 10) {
        line += 1;
        return finish(scan + 2, AT_BREAK);
      }
      if (parts === null) parts = [head];
      parts.push(text.slice(segFrom, scan));
      appended += scan - segFrom;
      note('stray_cr');
      scan += 1;
      segFrom = scan;
    }
  }

  // Reads a field whose opening quote is at pos. Sets fValue and fEnd like readPlain does.
  function readQuoted() {
    const openPos = pos;
    const openLine = line;
    let scan = pos + 1;
    let segFrom = scan; // start of the plain stretch inside the quotes
    let parts = null; // field pieces, allocated only when an escape or a CR splits the field

    for (;;) {
      if (scan >= len) {
        // The quote never closes: it is a literal character, the line count goes back to what
        // it was at the opening quote, and the text after the quote is read again as unquoted.
        pos = openPos + 1;
        line = openLine;
        note('unterminated_quote');
        readPlain('"');
        return;
      }
      const cc = text.charCodeAt(scan);
      if (cc === 34) {
        if (text.charCodeAt(scan + 1) === 34) {
          // A doubled quote inside the field is one literal quote.
          if (parts === null) parts = [];
          parts.push(text.slice(segFrom, scan), '"');
          scan += 2;
          segFrom = scan;
          continue;
        }
        // The closing quote: the field's content is everything scanned so far.
        let body;
        if (parts === null) {
          body = text.slice(segFrom, scan);
        } else {
          parts.push(text.slice(segFrom, scan));
          body = parts.join('');
        }
        scan += 1;
        pos = scan;
        if (pos >= len) {
          fValue = body;
          fEnd = AT_END;
          return;
        }
        const after = text[pos];
        if (after === ',') {
          pos += 1;
          fValue = body;
          fEnd = AT_COMMA;
          return;
        }
        if (after === '\n') {
          pos += 1;
          line += 1;
          fValue = body;
          fEnd = AT_BREAK;
          return;
        }
        if (after === '\r' && text[pos + 1] === '\n') {
          pos += 2;
          line += 1;
          fValue = body;
          fEnd = AT_BREAK;
          return;
        }
        // Anything else after the closing quote is junk, appended as it stands up to the next
        // comma or line break; it is reported when the field ends, if it appended anything.
        if (readPlain(body) > 0) note('quote_junk');
        return;
      }
      if (cc === 13) {
        // A CRLF or a lone CR inside quotes reads as a single LF.
        if (parts === null) parts = [];
        parts.push(text.slice(segFrom, scan), '\n');
        line += 1;
        scan += text.charCodeAt(scan + 1) === 10 ? 2 : 1;
        segFrom = scan;
        continue;
      }
      if (cc === 10) line += 1; // an LF inside quotes is field text, and one more line
      scan += 1;
    }
  }

  for (;;) {
    // Lines with no characters are no records: skip bare LF and CRLF line ends.
    while (pos < len) {
      const c = text[pos];
      if (c === '\n') {
        pos += 1;
        line += 1;
      } else if (c === '\r' && text[pos + 1] === '\n') {
        pos += 2;
        line += 1;
      } else {
        break;
      }
    }
    if (pos >= len) break;

    recLine = line;
    recRow = header && columns === null ? null : rows.length;

    const fields = [];
    let lastQuoted = false;
    for (;;) {
      if (text[pos] === '"') {
        readQuoted();
        lastQuoted = true;
      } else {
        readPlain('');
        lastQuoted = false;
      }
      fields.push(fValue);
      if (fEnd !== AT_COMMA) break;
    }

    // A line that held only dropped stray CRs is left with no characters: no record (the
    // stray_cr errors it produced stay).
    if (fields.length === 1 && fields[0] === '' && !lastQuoted) continue;

    if (width < 0) width = fields.length;
    if (header && columns === null) {
      columns = fields;
      continue;
    }
    if (fields.length < width) {
      errors.push({ code: 'short_row', line: recLine, row: recRow, fields: fields.length });
      while (fields.length < width) fields.push('');
    } else if (fields.length > width) {
      errors.push({ code: 'long_row', line: recLine, row: recRow, extra: fields.splice(width) });
    }
    rows.push(fields);
  }

  return { columns: header ? columns ?? [] : null, rows, errors };
}
