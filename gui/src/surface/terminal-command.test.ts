import { expect, it } from "vitest";
import { read } from "./commands";
import { applyPaneCommand } from "./pane-command";
import { oneP, SESSION_PANE } from "./split-model";
import { terminalArguments } from "./terminal-command";

const splits = ['/lsplit', '/rsplit', '/tsplit', '/bsplit', '/lsplitx', '/rsplitx', '/tsplitx', '/bsplitx', '/split', '/split right', '/split down'];
const direction = (split: string) => ({ l: 'left', r: 'right', t: 'up', b: 'down' } as const)[split === '/split' || split.endsWith('right') ? 'r' : split.endsWith('down') ? 'b' : split[1] as 'l' | 'r' | 't' | 'b'];

it.each(['l', 'r', 't', 'b'])('accepts named terminal contexts in /%ssplit and focus variants', direction => {
  for (const focus of ['', 'x']) {
    expect(read(`/${direction}split${focus} xterm env:DEV target:api`)).toMatchObject({
      kind: 'directional-split', takeFocus: !!focus, content: { terminal: true, environment: 'DEV', target: 'api' },
    });
  }
});
it('normalizes the long split form and preserves quoted literal boundaries', () => {
  expect(read('/split right xterm target:"api worker" env:"dev team"')).toEqual(read('/rsplit xterm target:"api worker" env:"dev team"'));
  expect(terminalArguments('env:"dev team" target:"a\\\"b\\\\c\\n\\r\\t"')).toEqual({ environment: 'dev team', target: 'a"b\\c\n\r\t' });
  for (const value of ['dev', 'run-id', 'Summary', 'http://localhost:8771', '/tmp/file', 'a\u00a0b'])
    expect(terminalArguments(`env:${value}`)).toEqual({ environment: value });
});
it.each(['env=DEV', 'env:', 'env:""', 'env:a env:b', 'target:a target:b', 'other:a', 'env:$value', 'env:a|b', 'env:a>b', 'env:{x}', 'env:a\\b', 'env:a"b', 'env:"dev', 'env:"a\\q"', 'env:"a"target:b', 'extra'])('refuses invalid terminal options: %s', options => {
  expect(read(`/rsplit xterm ${options}`).kind).toBe('trouble');
});
it.each(splits)('reserves plain and quoted xterm for a terminal in %s', split => {
  const base = { kind: 'directional-split', direction: direction(split), takeFocus: split.endsWith('x') };
  for (const word of ['xterm', '"xterm"']) {
    expect(read(`${split} ${word}`)).toEqual({ ...base, content: { terminal: true } });
    expect(read(`${split} ${word} env:"dev  team" target:"a\\"b\\\\c\\n\\t"`)).toEqual({ ...base, content: { terminal: true, environment: 'dev  team', target: 'a"b\\c\n\t' } });
  }
});
it('opens /tab xterm and /tabx xterm as terminal tabs with an optional pane and exact literals', () => {
  for (const [tab, activate] of [['/tab', false], ['/tabx', true]] as const) {
    for (const word of ['xterm', '"xterm"']) {
      expect(read(`${tab} ${word}`)).toEqual({ kind: 'terminal-tab', activate });
      expect(read(`${tab} ${word} pane:p2`)).toEqual({ kind: 'terminal-tab', activate, pane: 'p2' });
      expect(read(`${tab} ${word} target:"api  worker" pane:p12 env:"a\\"b\\\\c\\r"`)).toEqual({ kind: 'terminal-tab', activate, pane: 'p12', target: 'api  worker', environment: 'a"b\\c\r' });
    }
  }
});
it('keeps the /terminal-tab shape and refuses a pane there', () => {
  expect(read('/terminal-tab')).toEqual({ kind: 'terminal-tab' });
  expect(read('/terminal-tab env:"dev  team"')).toEqual({ kind: 'terminal-tab', environment: 'dev  team' });
  expect(read('/terminal-tab pane:p1').kind).toBe('trouble');
});
it('keeps names that only contain xterm as workspaces and $xterm as a value', () => {
  expect(read('/tab xterm-team')).toEqual({ kind: 'workspace-tab', workspace: 'xterm-team', activate: false });
  expect(read('/tabx xterm-team pane:p1')).toEqual({ kind: 'workspace-tab', workspace: 'xterm-team', pane: 'p1', activate: true });
  expect(read('/tab $xterm')).toEqual({ kind: 'value-tab', node: 'xterm', activate: false });
  expect(read('/tabx $xterm pane:p2')).toEqual({ kind: 'value-tab', node: 'xterm', pane: 'p2', activate: true });
  expect(read('/rsplit xterm-team')).toMatchObject({ content: { workspace: 'xterm-team' } });
  expect(read('/split $xterm')).toMatchObject({ direction: 'right', content: { value: 'xterm' } });
});
it.each([
  '/tab xterm extra', '/tab xterm pane:p0', '/tab xterm pane:p', '/tab xterm pane:1', '/tab xterm pane:"p1"', '/tab xterm pane:p1x',
  '/tab xterm pane:p1 pane:p2', '/tab xterm env:a env:b', '/tab xterm target:a target:b', '/tabx xterm env:$value', '/tabx xterm env:"dev',
  '/tab xterm env:"a"pane:p1', '/tab "\\u0078term"', '/tab xterm related',
  ...splits.flatMap(split => [`${split} xterm pane:p1`, `${split} "xterm" pane:p2`, `${split} xterm extra`, `${split} "\\u0078term"`]),
])('refuses malformed terminal commands before dispatch: %s', line => {
  const typed = read(line);
  expect(typed.kind).toBe('trouble');
  expect(() => applyPaneCommand(oneP(SESSION_PANE), line, 'p1', shown => shown)).toThrow(typed.kind === 'trouble' ? typed.said : '');
});
