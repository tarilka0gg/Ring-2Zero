// Behaviour test for client.html's createInputCapture, on a fake DOM.
// Run: node docs/client-examples/input-capture.test.js
const fs = require('fs'), assert = require('assert');
const html = fs.readFileSync(require('path').join(__dirname, 'client.html'), 'utf8');
const src = html.slice(html.indexOf('function createInputCapture'), html.indexOf('connect();\n</script>'));
function target() { const l = {}; return { l, addEventListener(e, f) { (l[e] ||= []).push(f); }, removeEventListener(e, f) { l[e] = (l[e] || []).filter(x => x !== f); }, fire(e, ev) { (l[e] || []).forEach(f => f({ type: e, preventDefault() { this.pd = true; }, ...ev })); } }; }
const win = target(), doc = target(); doc.visibilityState = 'visible';
let rafs = []; const ctx = { window: win, document: doc, requestAnimationFrame: f => (rafs.push(f), rafs.length), cancelAnimationFrame: id => { rafs[id - 1] = null; } };
const createInputCapture = new Function(...Object.keys(ctx), src + '; return createInputCapture;')(...Object.values(ctx));
const canvas = Object.assign(target(), { setPointerCapture() {} });
const sent = []; const hex = b => Buffer.from(b).toString('hex');
const cap = createInputCapture({ canvas, getGeometry: () => ({ dst: { x: 100, y: 0, w: 800, h: 450 }, dpr: 2 }), send: b => sent.push(hex(b)) });
const runRaf = () => { const r = rafs; rafs = []; r.forEach(f => f && f()); };

canvas.fire('pointermove', { offsetX: 100, offsetY: 100 }); assert.equal(sent.length, 0, 'disabled: nothing sent');
cap.enable();
// motion coalescing: 3 moves → 1 message on rAF, last position wins
canvas.fire('pointermove', { offsetX: 60, offsetY: 10 });
canvas.fire('pointermove', { offsetX: 250, offsetY: 112.5 });
canvas.fire('pointermove', { offsetX: 500, offsetY: 225 }); // (1000-100)/800 >1 → clamp 65535; 450/450 → 65535
assert.equal(sent.length, 0); runRaf();
assert.deepEqual(sent, ['01ffffffff']); sent.length = 0;
canvas.fire('pointermove', { offsetX: 250, offsetY: 112.5 }); // (500-100)/800=.5 → 32768 (0x8000); 225/450=.5
// button flushes pending motion first
canvas.fire('mousedown', { button: 0, offsetX: 250, offsetY: 112.5 });
assert.deepEqual(sent, ['0100800080', '020001']); sent.length = 0;
// chorded right button while left held; redundant mousedown ignored
canvas.fire('mousedown', { button: 2, offsetX: 250, offsetY: 112.5 });
canvas.fire('mousedown', { button: 2, offsetX: 250, offsetY: 112.5 });
assert.deepEqual(sent, ['0100800080', '020201']); sent.length = 0;
// keys: 4-byte messages, autorepeat dropped, unknown ignored
win.fire('keydown', { code: 'KeyA' }); win.fire('keydown', { code: 'KeyA', repeat: true });
win.fire('keydown', { code: 'ShiftLeft' }); win.fire('keydown', { code: 'LaunchMail' });
assert.deepEqual(sent, ['041e0001', '042a0001']); sent.length = 0;
// wheel: lines ×16
canvas.fire('wheel', { deltaMode: 1, deltaX: 0, deltaY: -3 });
assert.deepEqual(sent, ['030000d0ff']); sent.length = 0;
// blur releases everything held
win.fire('blur', {});
assert.deepEqual(sent.sort(), ['020000', '020200', '041e0000', '042a0000'].sort()); sent.length = 0;
win.fire('keyup', { code: 'KeyA' }); assert.equal(sent.length, 0, 'no release for an unheld key');
// disable detaches listeners
cap.disable(); win.fire('keydown', { code: 'KeyB' }); assert.equal(sent.length, 0);
// keymap spot checks against linux/input-event-codes.h
const km = src.match(/const KEYMAP = \{([\s\S]*?)\};/)[1];
const K = Object.fromEntries([...km.matchAll(/(\w+): (\d+)/g)].map(m => [m[1], +m[2]]));
const expect = { Escape: 1, Digit0: 11, KeyQ: 16, KeyA: 30, KeyZ: 44, Enter: 28, Space: 57, F10: 68, F11: 87, F12: 88, Numpad0: 82, Numpad5: 76, Numpad7: 71, NumpadDecimal: 83, NumpadEnter: 96, AltRight: 100, ArrowUp: 103, Delete: 111, MetaLeft: 125, ContextMenu: 127, IntlBackslash: 86 };
for (const [k, v] of Object.entries(expect)) assert.equal(K[k], v, k);
assert.equal(new Set(Object.values(K)).size, Object.keys(K).length, 'no duplicate evdev codes');
console.log(`input capture: all checks passed (${Object.keys(K).length} keys mapped)`);
