import type { DiffHunk } from '../generated/DiffHunk';
import type { DiffLine } from '../generated/DiffLine';

export type Segment = { text: string; changed: boolean };
export type LineSegments = Map<string, Segment[]>;

// A bounded LCS avoids quadratic work on generated or minified lines. Tokens retain
// every byte of whitespace and punctuation, so joining segments reproduces the input.
const MAX_LINES = 12;
const MAX_TOKENS = 200;
const MAX_CHARS = 2000;
const graphemes = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
const word = /^[\p{L}\p{N}_]/u;
const whitespace = /^\s/u;

function tokens(text: string): string[] {
  const parts: string[] = [];
  let previous = '';
  for (const { segment } of graphemes.segment(text)) {
    const kind = word.test(segment) ? 'word' : whitespace.test(segment) ? 'space' : '';
    if (kind && kind === previous) parts[parts.length - 1] += segment;
    else parts.push(segment);
    if (parts.length > MAX_TOKENS) break;
    previous = kind;
  }
  return parts;
}

export function changedSegments(oldText: string, newText: string): [Segment[], Segment[]] | null {
  if (oldText.length > MAX_CHARS || newText.length > MAX_CHARS) return null;
  const old = tokens(oldText);
  const next = tokens(newText);
  if (old.length > MAX_TOKENS || next.length > MAX_TOKENS) return null;
  const dp = Array.from({ length: old.length + 1 }, () => new Uint16Array(next.length + 1));
  for (let i = old.length - 1; i >= 0; i--) for (let j = next.length - 1; j >= 0; j--)
    dp[i][j] = old[i] === next[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const sameOld = new Set<number>();
  const sameNew = new Set<number>();
  let i = 0, j = 0;
  while (i < old.length && j < next.length) {
    if (old[i] === next[j]) { sameOld.add(i++); sameNew.add(j++); }
    else if (dp[i + 1][j] >= dp[i][j + 1]) i++;
    else j++;
  }
  const segments = (parts: string[], same: Set<number>): Segment[] => {
    const result: Segment[] = [];
    parts.forEach((text, index) => {
      const changed = !same.has(index);
      if (result.at(-1)?.changed === changed) result[result.length - 1].text += text;
      else result.push({ text, changed });
    });
    return result;
  };
  return [segments(old, sameOld), segments(next, sameNew)];
}

export function hunkWordChanges(hunk: DiffHunk, budget = { pairs: 200 }): LineSegments {
  const result: LineSegments = new Map();
  const lines = hunk.lines;
  for (let i = 0; i < lines.length;) {
    if (lines[i].kind !== 'deletion') { i++; continue; }
    const deleted: DiffLine[] = [];
    const added: DiffLine[] = [];
    while (lines[i]?.kind === 'deletion') deleted.push(lines[i++]);
    while (lines[i]?.kind === 'addition') added.push(lines[i++]);
    if (!added.length || deleted.length > MAX_LINES || added.length > MAX_LINES) continue;
    for (let pair = 0; pair < Math.min(deleted.length, added.length); pair++) {
      if (budget.pairs === 0) return result;
      budget.pairs--;
      const change = changedSegments(deleted[pair].text, added[pair].text);
      if (change) { result.set(deleted[pair].id, change[0]); result.set(added[pair].id, change[1]); }
    }
  }
  return result;
}
