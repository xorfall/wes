/** Slash commands are client-owned; named text uses the engine language's lexical rules. */
export type TerminalIntent = { environment?: string; target?: string };
/** `/tab xterm` and `/tabx xterm` may also name the pane that receives the terminal. */
export type TerminalTabIntent = TerminalIntent & { pane?: string };
const whitespace = /[\u0009-\u000d\u001c-\u0020\u1680\u2000-\u2006\u2008-\u200a\u2028\u2029\u205f\u3000]/;
const escapes: Record<string, string> = { '"': '"', '\\': '\\', n: '\n', r: '\r', t: '\t' };
const terminalWords = ['xterm', '"xterm"'];

/** Parse the tail after `xterm`, without interpreting quoted values as commands. */
export function terminalArguments(source: string): TerminalIntent {
  return parse(source, 0, false);
}

/** Parse the tail after a split command's name: `xterm` or `"xterm"`, then its arguments. */
export function terminalCommand(source: string): TerminalIntent {
  return parse(source, afterTerminalWord(source), false);
}

/** Parse the tail after `/tab` or `/tabx`: `xterm` or `"xterm"`, then its arguments and an optional pane. */
export function terminalTabCommand(source: string): TerminalTabIntent {
  return parse(source, afterTerminalWord(source), true);
}

function afterTerminalWord(source: string): number {
  let at = 0;
  while (at < source.length && whitespace.test(source[at]!)) at++;
  const word = terminalWords.find(it => source.startsWith(it, at));
  const end = at + (word?.length ?? 0);
  if (!word || (end < source.length && !whitespace.test(source[end]!))) throw new Error('Write xterm as a plain word or as "xterm".');
  return end;
}

function parse(source: string, from: number, panes: boolean): TerminalTabIntent {
  const result: TerminalTabIntent = {};
  let at = from;
  const skip = () => { while (at < source.length && whitespace.test(source[at]!)) at++; };
  skip();
  while (at < source.length) {
    const option = (panes ? /^(env|target|pane):/ : /^(env|target):/).exec(source.slice(at));
    if (!option) throw new Error(panes ? 'Use /tab xterm or /tabx xterm [pane:pN] [env:NAME] [target:NAME]; named arguments use a colon.'
      : 'Use xterm [env:NAME] [target:NAME]; named arguments use a colon.');
    const key = option[1] === 'env' ? 'environment' : option[1] === 'target' ? 'target' : 'pane';
    if (result[key] !== undefined) throw new Error(`Repeated ${option[1]}: argument.`);
    at += option[0].length;
    let value = '';
    if (key === 'pane') {
      const start = at;
      while (at < source.length && !whitespace.test(source[at]!)) at++;
      value = source.slice(start, at);
      if (!/^p[1-9][0-9]*$/.test(value)) throw new Error('pane: takes a pane id such as p1.');
    } else if (source[at] === '"') {
      at++;
      while (at < source.length && source[at] !== '"') {
        const ch = source[at++]!;
        if (ch !== '\\') value += ch;
        else {
          const escape = source[at++];
          if (escape === undefined || !Object.hasOwn(escapes, escape)) throw new Error('Invalid quoted text escape; use \\" or \\\\ or \\n, \\r, \\t.');
          value += escapes[escape];
        }
      }
      if (source[at++] !== '"') throw new Error('Unterminated quoted text in terminal command.');
      if (at < source.length && !whitespace.test(source[at]!)) throw new Error('Separate terminal arguments with whitespace.');
    } else {
      const start = at;
      while (at < source.length && !whitespace.test(source[at]!)) at++;
      value = source.slice(start, at);
      if (value.startsWith('$') || /[>|{}"\\]/.test(value)) throw new Error('Use literal named text; quote values containing spaces or syntax characters.');
    }
    if (!value) throw new Error(`${option[1]}: requires a nonempty name.`);
    result[key] = value;
    skip();
  }
  return result;
}
