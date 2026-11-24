import { useEffect, useMemo, useRef, useState } from 'react';
import type { ChangedFile } from '../generated/ChangedFile';
import type { FileState } from '../generated/FileState';
import type { QueueView } from '../generated/QueueView';
import type { ExclusionRule } from '../generated/ExclusionRule';
import { extensionCounts, extensionLabel, extensionOf, navigationRows, visibleEntries, type NavigationView, type ReviewFilter } from '../fileNavigation';
import { Button } from '../components/ui/button';
import { Input } from '../components/ui/input';

type ScopeTarget = { kind: 'file' | 'directory' | 'glob'; expression: string; action: 'include' | 'exclude'; lifetime: 'review' | 'repository' };
type TypePreference = 'default' | 'include' | 'exclude';
export default function FileNavigator({ files, states, queue = null, rules = [], typePreferences = {}, selectedId, pending = false, error = null, onRetry, onSelect, onScope, onTypePreference }: {
  files: ChangedFile[]; states: FileState[]; queue?: QueueView | null; rules?: ExclusionRule[];
  typePreferences?: Record<string, TypePreference>; selectedId?: string; pending?: boolean; error?: string | null; onRetry?: () => void;
  onSelect: (id: string) => void; onScope: (target: ScopeTarget) => void; onTypePreference: (extension: string, mode: TypePreference) => void;
}) {
  const [query, setQuery] = useState('');
  const [regex, setRegex] = useState(false);
  const [selectedExtensions, setSelectedExtensions] = useState<string[] | null>(null);
  const [view, setView] = useState<NavigationView>('tree');
  const [review, setReview] = useState<ReviewFilter>('all');
  const [showIgnored, setShowIgnored] = useState(false);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [menu, setMenu] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState(new Set<string>());
  const navRef = useRef<HTMLElement>(null);
  const filterTrigger = useRef<HTMLButtonElement>(null);
  const menuTrigger = useRef<HTMLButtonElement>(null);
  const counts = useMemo(() => extensionCounts(files), [files]);
  const extensions = selectedExtensions ?? counts.map(({ extension }) => extension);
  const rulesById = new Map(rules.map((rule) => [rule.id, rule]));
  const result = visibleEntries(files, states, queue, { query, regex, extensions, view, review, showIgnored });
  const rows = navigationRows(result.entries, view, collapsed);
  const closePanels = (restore = false) => {
    if (restore) (menu ? menuTrigger : filterTrigger).current?.focus();
    setMenu(null); setFiltersOpen(false);
  };
  useEffect(() => {
    const keydown = (event: KeyboardEvent) => { if (event.key === 'Escape') { event.preventDefault(); closePanels(true); } };
    const pointerdown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Element) || !navRef.current?.contains(target) || !target.closest('.popover, .menu, .filter-button, .more')) closePanels();
    };
    window.addEventListener('keydown', keydown); window.addEventListener('pointerdown', pointerdown);
    return () => { window.removeEventListener('keydown', keydown); window.removeEventListener('pointerdown', pointerdown); };
  });
  const toggleMenu = (key: string, trigger: HTMLButtonElement) => {
    if (menu === key) { setMenu(null); trigger.focus(); }
    else { menuTrigger.current = trigger; setMenu(key); setFiltersOpen(false); }
  };
  const scope = (kind: ScopeTarget['kind'], expression: string, action: ScopeTarget['action'], lifetime: ScopeTarget['lifetime']) => {
    onScope({ kind, expression, action, lifetime }); setMenu(null); menuTrigger.current?.focus();
  };
  const description = (state: FileState | undefined) => {
    if (!state?.scope.ignored) return '';
    const rule = state.scope.effectiveRuleId ? rulesById.get(state.scope.effectiveRuleId) : undefined;
    return rule ? ` — excluded by ${rule.kind} rule ${rule.expression}` : ' — excluded from review';
  };
  const actions = (kind: 'file' | 'directory', path: string) => <div className="menu absolute right-1 top-[25px] z-20 grid min-w-[150px] rounded border border-line bg-surface p-2 shadow-lg" role="group" aria-label={`Scope actions for ${kind === 'file' ? 'file' : 'folder'} ${path}`}>
    {(['include', 'exclude'] as const).flatMap((action) => (['review', 'repository'] as const).map((lifetime) => <Button key={`${action}-${lifetime}`} variant="ghost" size="xs" type="button" disabled={pending} className="justify-start" onClick={() => scope(kind, path, action, lifetime)}>{action === 'include' ? 'Include' : 'Exclude'} {lifetime === 'review' ? 'this review' : 'always'}</Button>))}
  </div>;
  return <nav ref={navRef} className="navigator relative min-w-0 text-[13px]" aria-label="Changed files">
    <div className="toolbar flex items-center gap-1 p-1.5">
      <Input type="search" aria-label="Search file paths" placeholder="Search paths" className="min-w-0 flex-1 h-7" value={query} onChange={(event) => setQuery(event.target.value)} />
      <label className="regex whitespace-nowrap text-[11px]"><input type="checkbox" checked={regex} onChange={(event) => setRegex(event.target.checked)} /> Regex</label>
      <Button ref={filterTrigger} type="button" variant="outline" size="xs" className="filter-button" aria-label="File filters" aria-expanded={filtersOpen} onClick={() => { setFiltersOpen(!filtersOpen); setMenu(null); }}>Filters</Button>
    </div>
    {result.error && <p role="alert" className="error mx-2 text-destructive">{result.error}</p>}
    {error && <div role="alert" className="error mx-2 text-destructive">{error} {onRetry && <Button type="button" variant="outline" size="xs" onClick={onRetry}>Retry scope change</Button>}</div>}
    {filtersOpen && <div className="popover absolute right-1 z-10 grid w-[min(340px,95%)] max-h-[60vh] gap-2 overflow-auto rounded border border-line bg-surface p-2 shadow-lg" aria-label="File filters">
      <fieldset><legend className="font-semibold">File types</legend>{counts.map(({ extension, count }) => <div className="type-row flex justify-between gap-1 py-[3px]" key={extension}>
        <label><input type="checkbox" checked={extensions.includes(extension)} onChange={() => setSelectedExtensions(extensions.includes(extension) ? extensions.filter((value) => value !== extension) : [...extensions, extension])} /> {extensionLabel(extension)} <small>({count})</small></label>
        <select className="max-w-[125px] rounded border border-line bg-surface" aria-label={`Repository preference for ${extensionLabel(extension)}`} disabled={pending} value={typePreferences[extension] ?? 'default'} onChange={(event) => onTypePreference(extension, event.target.value as TypePreference)}><option value="default">Default</option><option value="include">Always include</option><option value="exclude">Always exclude</option></select>
      </div>)}</fieldset>
      <label>View <select value={view} onChange={(event) => setView(event.target.value as NavigationView)}><option value="tree">Tree</option><option value="priority">Priority</option></select></label>
      <label>Review status <select value={review} onChange={(event) => setReview(event.target.value as ReviewFilter)}><option value="all">All</option><option value="unreviewed">Unreviewed</option><option value="reviewed-current">Reviewed</option><option value="changed-since-reviewed">Changed since review</option></select></label>
      <label><input type="checkbox" checked={showIgnored} onChange={(event) => setShowIgnored(event.target.checked)} /> Show ignored</label>
    </div>}
    <ul className="rows m-0 list-none p-0">{rows.map((row) => row.kind === 'directory' ? <li key={`d:${row.path}`} className="row relative flex h-7 items-center hover:bg-surface focus-within:bg-surface" style={{ paddingLeft: row.depth * 14 }}>
      <Button variant="ghost" type="button" className="main h-full min-w-0 flex-1 justify-start gap-1 p-0 text-left" aria-label={`${collapsed.has(row.path) ? 'Expand' : 'Collapse'} folder ${row.path}`} aria-expanded={!collapsed.has(row.path)} onClick={() => setCollapsed((old) => { const next = new Set(old); if (next.has(row.path)) next.delete(row.path); else next.add(row.path); return next; })}><span className="chevron w-3 shrink-0" aria-hidden="true">{collapsed.has(row.path) ? '▸' : '▾'}</span><svg aria-hidden="true" width="15" height="14" viewBox="0 0 16 14" fill="none" stroke="currentColor" strokeWidth="1.3"><path d="M1 3V1h5l2 2h7v10H1z" /></svg><span className="name truncate" title={row.path}>{row.name}</span><small>{row.count}</small></Button>
      <Button variant="ghost" size="icon-xs" className="more" type="button" aria-label={`Actions for folder ${row.path}`} aria-expanded={menu === `d:${row.path}`} onClick={(event) => toggleMenu(`d:${row.path}`, event.currentTarget)}>⋯</Button>
      {menu === `d:${row.path}` && actions('directory', row.path)}
    </li> : (() => { const file = row.entry.file; const ignored = row.entry.state?.scope.ignored ?? false; const code = ['.ts', '.tsx', '.js', '.jsx', '.rs', '.py', '.go'].includes(extensionOf(file.path)); return <li key={`f:${file.id}`} className={`row relative flex h-7 items-center hover:bg-surface focus-within:bg-surface${selectedId === file.id ? ' selected bg-accent-soft' : ''}${ignored ? ' ignored opacity-60' : ''}`} style={{ paddingLeft: row.depth * 14 }}>
      <Button variant="ghost" type="button" className="main h-full min-w-0 flex-1 justify-start gap-1 p-0 text-left" aria-current={selectedId === file.id ? 'true' : undefined} aria-label={`Open ${file.path}${description(row.entry.state)}`} title={`${file.path}${description(row.entry.state)}`} onClick={() => onSelect(file.id)}><span className="chevron w-3 shrink-0" aria-hidden="true"/><span className={`icon shrink-0 ${code ? 'text-accent' : 'text-muted'}`} aria-hidden="true"><svg width="14" height="16" viewBox="0 0 14 16" fill="none" stroke="currentColor" strokeWidth="1.3"><path d="M2 1h6l4 4v10H2z M8 1v4h4"/>{code ? <path d="m6 8-2 2 2 2m2-4 2 2-2 2"/> : <path d="M4 9h5M4 12h4"/>}</svg></span><span className="name truncate">{view === 'priority' ? file.path : file.path.split('/').at(-1)}</span></Button>
      <Button variant="ghost" size="icon-xs" className="more" type="button" aria-label={`Actions for file ${file.path}`} aria-expanded={menu === `f:${file.id}`} onClick={(event) => toggleMenu(`f:${file.id}`, event.currentTarget)}>⋯</Button>
      {menu === `f:${file.id}` && actions('file', file.path)}
    </li>; })())}</ul>
  </nav>;
}
