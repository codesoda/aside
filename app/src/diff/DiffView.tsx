import { useEffect, useMemo, useRef, type ReactNode } from 'react';
import type { ChangedFile } from '../generated/ChangedFile';
import type { DiffHunk } from '../generated/DiffHunk';
import type { DiffLine } from '../generated/DiffLine';
import type { Side } from '../generated/Side';
import { hunkWordChanges } from './wordDiff';
import './diff.css';

export type DiffThreadMarker = { id: string; side: Side | null; line: number | null; startLine?: number | null; anchorState?: string; unreadCount?: number; lastParticipant?: string | null };
export type DiffFindingMarker = { id: string; side: Side; line: number };
export type DiffViewProps = {
  diff: ChangedFile;
  threads?: DiffThreadMarker[];
  findings?: DiffFindingMarker[];
  selectedThreadId?: string | null;
  selectedFindingId?: string | null;
  readOnly?: boolean;
  onSelectLine?: (line: DiffLine) => void;
  onOpenMarker?: (threads: DiffThreadMarker[], findings: DiffFindingMarker[]) => void;
  onExplain?: (hunk: DiffHunk, line: DiffLine | undefined, trigger: HTMLButtonElement) => void;
  renderAfterLine?: (line: DiffLine) => ReactNode;
};

export function lineSide(line: DiffLine): Side | null {
  return line.kind === 'meta' ? null : line.kind === 'deletion' ? 'LEFT' : 'RIGHT';
}

function copySource(event: ClipboardEvent, root: HTMLDivElement) {
  if (!event.clipboardData || (event.target instanceof Element && event.target.closest('input, textarea, [contenteditable=true]'))) return;
  const selection = root.ownerDocument.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount !== 1) return;
  const range = selection.getRangeAt(0);
  const sourceElement = (node: Node) => (node instanceof Element ? node : node.parentElement)?.closest('.diff-code');
  const first = sourceElement(range.startContainer);
  const last = sourceElement(range.endContainer);
  if (!first || !last || !root.contains(first) || !root.contains(last)) return;
  const text: string[] = [];
  for (const code of root.querySelectorAll('.diff-code')) {
    if (!range.intersectsNode(code)) continue;
    const portion = range.cloneRange();
    portion.selectNodeContents(code);
    if (code === first) portion.setStart(range.startContainer, range.startOffset);
    if (code === last) portion.setEnd(range.endContainer, range.endOffset);
    text.push(portion.toString());
  }
  event.clipboardData.setData('text/plain', text.join('\n'));
  event.preventDefault();
}

export function DiffView({ diff, threads = [], findings = [], selectedThreadId, selectedFindingId, readOnly = false, onSelectLine, onOpenMarker, onExplain, renderAfterLine }: DiffViewProps) {
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const doc = root.current?.ownerDocument ?? document;
    const copy = (event: ClipboardEvent) => { if (root.current) copySource(event, root.current); };
    doc.addEventListener('copy', copy);
    return () => doc.removeEventListener('copy', copy);
  }, []);
  const changesByHunk = useMemo(() => {
    const budget = { pairs: 200 };
    return diff.hunks.map((hunk) => hunkWordChanges(hunk, budget));
  }, [diff.hunks]);
  if (diff.isBinary || !diff.hunks.length) return <p className="diff-empty">{diff.isBinary ? 'Binary file: no text diff.' : 'No text hunks available for this file.'}</p>;
  return <div className="workspace-code diff-view" aria-label={`Unified diff for ${diff.path}`} ref={root}>
    {diff.hunks.map((hunk, index) => {
      const changes = changesByHunk[index];
      return <section className="diff-hunk" key={hunk.id} aria-label={`Hunk at old line ${hunk.oldStart}, new line ${hunk.newStart}`}>
        <div className="diff-hunk-header"><span>@@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},{hunk.newCount} @@ {hunk.header}</span>
          {onExplain && <button type="button" disabled={!diff.patchComplete} onClick={(event) => onExplain(hunk, undefined, event.currentTarget)}>Explain this change</button>}
        </div>
        {hunk.lines.map((line) => {
          const side = lineSide(line);
          const number = side === 'LEFT' ? line.oldLine : line.newLine;
          const numberOn = (markerSide: Side) => markerSide === 'LEFT' ? line.oldLine : line.newLine;
          const lineThreads = threads.filter((thread) => thread.anchorState === 'current' && thread.side !== null && thread.line !== null && (thread.startLine ?? thread.line) === numberOn(thread.side));
          const inRange = threads.some((thread) => {
            if (thread.id !== selectedThreadId || thread.anchorState !== 'current' || thread.side === null || thread.line === null) return false;
            const row = numberOn(thread.side);
            return row !== null && row >= (thread.startLine ?? thread.line) && row <= thread.line;
          });
          const lineFindings = findings.filter((finding) => numberOn(finding.side) !== null && finding.line === numberOn(finding.side));
          const segments = changes.get(line.id) ?? [{ text: line.text, changed: false }];
          return <div key={line.id}>
            <div className={`diff-row diff-${line.kind}${inRange ? ' diff-range' : ''}`} data-thread-anchor={lineThreads.map((thread) => thread.id).join(' ')}>
              <span className="diff-number" aria-label={line.oldLine === null ? undefined : `Old line ${line.oldLine}`}>{line.oldLine ?? ''}</span>
              <span className="diff-number" aria-label={line.newLine === null ? undefined : `New line ${line.newLine}`}>{line.newLine ?? ''}</span>
              <span className="diff-markers">{(['LEFT', 'RIGHT'] as const).map((markerSide) => {
                const markedThreads = lineThreads.filter((thread) => thread.side === markerSide);
                const markedFindings = lineFindings.filter((finding) => finding.side === markerSide);
                const count = markedThreads.length + markedFindings.length;
                if (!count || line.kind === 'meta') return null;
                const unread = markedThreads.reduce((total, thread) => total + (thread.unreadCount ?? 0), 0);
                return <span className="diff-marker-group" key={markerSide}><button type="button" className={`thread-marker marker-group${unread ? ' has-unread' : ''}`} style={{ '--participant': markedThreads.at(-1)?.lastParticipant === 'agent' ? '#7c3aed' : '#2563eb' } as React.CSSProperties}
                  aria-label={`Open ${count} ${count === 1 ? 'conversation' : 'conversations'} on ${markerSide === 'LEFT' ? 'old' : 'new'} line ${markerSide === 'LEFT' ? line.oldLine : line.newLine}; ${unread} unread; last ${markedThreads.at(-1)?.lastParticipant ?? 'insight'}`}
                  aria-pressed={markedThreads.some((thread) => thread.id === selectedThreadId) || markedFindings.some((finding) => finding.id === selectedFindingId)}
                  onClick={() => onOpenMarker?.(markedThreads, markedFindings)} />{count > 1 && <span>{count}</span>}</span>;
              })}</span>
              {side && diff.patchComplete && !readOnly && onSelectLine ? <button type="button" className="diff-draft" title="Draft a comment on this line" aria-label={`Draft a comment on ${side === 'LEFT' ? 'old' : 'new'} line ${number}`} onClick={() => onSelectLine(line)}>+</button> : <span className="diff-draft-space" />}
              {(line.kind === 'addition' || line.kind === 'deletion') && onExplain && <button type="button" className="diff-explain" disabled={!diff.patchComplete} aria-label={`Explain ${line.kind === 'deletion' ? 'removed old' : 'added new'} line ${number}`} onClick={(event) => onExplain(hunk, line, event.currentTarget)}>Explain</button>}
              <span className="sr-only">{line.kind === 'addition' ? 'Added: ' : line.kind === 'deletion' ? 'Removed: ' : ''}</span>
              <span className="diff-sign" aria-hidden="true">{line.kind === 'addition' ? '+' : line.kind === 'deletion' ? '-' : ' '}</span>
              <span className="diff-code" data-code-line={side ? number ?? undefined : undefined} data-side={side ?? undefined}>{segments.map((part, index) => part.changed ? <mark className="diff-word" key={index}>{part.text}</mark> : <span key={index}>{part.text}</span>)}</span>
            </div>
            {renderAfterLine?.(line)}
          </div>;
        })}
      </section>;
    })}
  </div>;
}
