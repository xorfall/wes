// Native test oracle: use the shipped xterm parser, without a browser or a PTY.
// Inputs are independently authored synthetic fixtures passed on stdin.
globalThis.self = globalThis;
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const { Terminal } = require(require.resolve('@xterm/xterm', { paths: [path.join(__dirname, '../../gui')] }));
const write = (terminal, bytes) => new Promise(resolve => terminal.write(Buffer.from(bytes, 'base64'), resolve));
function state(terminal) {
  const buffer = terminal.buffer.active;
  const rows = [];
  for (let i = 0; i < buffer.length; i++) {
    const line = buffer.getLine(i);
    // A rendered blank and an unused null cell are visually equivalent. Compare
    // meaningful text, row wrapping and every nonblank cell's drawing attributes.
    const text = line.translateToString(true).trimEnd();
    const cells = [];
    for (let col = 0; col < terminal.cols; col++) {
      const cell = line.getCell(col);
      if (cell.getChars().trim()) cells.push([col, cell.getChars(), cell.getWidth(), cell.getFgColorMode(), cell.getFgColor(), cell.getBgColorMode(), cell.getBgColor(), cell.isBold(), cell.isItalic(), cell.isUnderline(), cell.isInverse()]);
    }
    rows.push([text, line.isWrapped, cells]);
  }
  const { synchronizedOutputMode, ...modes } = terminal.modes;
  return { type: buffer.type, cursorX: buffer.cursorX, cursorY: buffer.cursorY, baseY: buffer.baseY, rows, modes };
}
(async () => {
  const fixtures = JSON.parse(fs.readFileSync(0, 'utf8'));
  for (const fixture of fixtures) {
    const reference = new Terminal({ cols: 80, rows: 24, scrollback: 5000, allowProposedApi: true });
    const restored = new Terminal({ cols: fixture.cols, rows: fixture.rows, scrollback: 5000, allowProposedApi: true });
    const replies = []; restored.onData(text => replies.push(text));
    for (const event of fixture.events) {
      if (event.resize) reference.resize(...event.resize);
      if (event.data) await write(reference, event.data);
    }
    await write(restored, fixture.snapshot);
    assert.equal(replies.length, 0, `${fixture.name}: recovery sent input/query replies`);
    // Both subsequent streams start at the exact same byte cursor, including
    // pending UTF-8 or VT sequences represented in the recovery.
    await write(reference, fixture.suffix); await write(restored, fixture.suffix);
    assert.deepEqual(state(restored), state(reference), fixture.name);
    if (restored.buffer.active.type === 'normal' && restored.buffer.active.baseY > 0) {
      restored.scrollToTop();
      assert.equal(restored.buffer.active.viewportY, 0, `${fixture.name}: scrollback inaccessible`);
    }
    reference.dispose(); restored.dispose();
    process.stdout.write(`PASS: ${fixture.name}\n`);
  }
})().catch(error => { process.stderr.write(String(error.stack)); process.exitCode = 1; });
