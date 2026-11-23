<script lang="ts">
  import type { ChangedFile } from '../generated/ChangedFile';
  import type { FileState } from '../generated/FileState';
  import type { QueueView } from '../generated/QueueView';
  import type { ExclusionRule } from '../generated/ExclusionRule';
  import { extensionCounts, extensionLabel, extensionOf, navigationRows, visibleEntries, type NavigationFilters, type ReviewFilter, type NavigationView } from '../fileNavigation';

  type ScopeTarget = { kind: 'file' | 'directory' | 'glob'; expression: string; action: 'include' | 'exclude'; lifetime: 'review' | 'repository' };
  type TypePreference = 'default' | 'include' | 'exclude';
  let { files, states, queue = null, rules = [], typePreferences = {}, selectedId, pending = false, error = null, onRetry, onSelect, onScope, onTypePreference }: {
    files: ChangedFile[]; states: FileState[]; queue?: QueueView | null; rules?: ExclusionRule[];
    typePreferences?: Record<string, TypePreference>; selectedId?: string;
    pending?: boolean; error?: string | null; onRetry?: () => void;
    onSelect: (id: string) => void;
    onScope: (target: ScopeTarget) => void;
    onTypePreference: (extension: string, mode: 'default' | 'include' | 'exclude') => void;
  } = $props();

  let query = $state('');
  let regex = $state(false);
  let selectedExtensions = $state<string[] | null>(null);
  let view = $state<NavigationView>('tree');
  let review = $state<ReviewFilter>('all');
  let showIgnored = $state(false);
  let filtersOpen = $state(false);
  let menu = $state<string | null>(null);
  let collapsed = $state(new Set<string>());
  let filterTrigger: HTMLButtonElement | null = null;
  let menuTrigger: HTMLButtonElement | null = null;
  let navigator: HTMLElement;
  const counts = $derived(extensionCounts(files));
  const extensions = $derived(selectedExtensions === null ? counts.map(({ extension }) => extension) : selectedExtensions);
  const rulesById = $derived(new Map(rules.map((rule) => [rule.id, rule])));
  const filters = $derived<NavigationFilters>({ query, regex, extensions, view, review, showIgnored });
  const result = $derived(visibleEntries(files, states, queue, filters));
  const rows = $derived(navigationRows(result.entries, view, collapsed));
  function toggleExtension(ext: string) {
    selectedExtensions = extensions.includes(ext) ? extensions.filter((value) => value !== ext) : [...extensions, ext];
  }
  function toggleFolder(path: string) {
    const next = new Set(collapsed);
    if (next.has(path)) next.delete(path); else next.add(path);
    collapsed = next;
  }
  function scope(kind: ScopeTarget['kind'], expression: string, action: ScopeTarget['action'], lifetime: ScopeTarget['lifetime']) {
    onScope({ kind, expression, action, lifetime });
    menu = null;
    menuTrigger?.focus();
  }
  function closePanels(restoreFocus = false) {
    if (restoreFocus) (menu ? menuTrigger : filterTrigger)?.focus();
    menu = null;
    filtersOpen = false;
  }
  function toggleMenu(key: string, trigger: HTMLButtonElement) {
    if (menu === key) { menu = null; trigger.focus(); }
    else { menu = key; menuTrigger = trigger; filtersOpen = false; }
  }
  function exclusionDescription(state: FileState | undefined): string {
    if (!state?.scope.ignored) return '';
    const rule = state.scope.effectiveRuleId ? rulesById.get(state.scope.effectiveRuleId) : undefined;
    return rule ? ` — excluded by ${rule.kind} rule ${rule.expression}` : ' — excluded from review';
  }
</script>

<svelte:window onkeydown={(event) => { if (event.key === 'Escape' && (menu || filtersOpen)) { event.preventDefault(); closePanels(true); } }} onpointerdown={(event) => { if (navigator && (!navigator.contains(event.target as Node) || !(event.target as Element).closest('.popover, .menu, .filter-button, .more'))) closePanels(); }} />
<nav bind:this={navigator} class="navigator" aria-label="Changed files">
  <div class="toolbar">
    <input type="search" aria-label="Search file paths" placeholder="Search paths" bind:value={query} />
    <label class="regex"><input type="checkbox" bind:checked={regex} /> Regex</label>
    <button type="button" class="filter-button" aria-label="File filters" aria-expanded={filtersOpen} onclick={(event) => { filterTrigger = event.currentTarget; filtersOpen = !filtersOpen; menu = null; }}>Filters</button>
  </div>
  {#if result.error}<p role="alert" class="error">{result.error}</p>{/if}
  {#if error}<div role="alert" class="error">{error} {#if onRetry}<button type="button" onclick={onRetry}>Retry scope change</button>{/if}</div>{/if}
  {#if filtersOpen}
    <div class="popover" aria-label="File filters">
      <fieldset><legend>File types</legend>
        {#each counts as { extension, count } (extension)}
          <div class="type-row">
            <label><input type="checkbox" checked={extensions.includes(extension)} onchange={() => toggleExtension(extension)} /> {extensionLabel(extension)} <small>({count})</small></label>
            <select aria-label={`Repository preference for ${extensionLabel(extension)}`} disabled={pending} value={typePreferences[extension] ?? 'default'} onchange={(event) => onTypePreference(extension, event.currentTarget.value as TypePreference)}>
              <option value="default">Default</option><option value="include">Always include</option><option value="exclude">Always exclude</option>
            </select>
          </div>
        {/each}
      </fieldset>
      <label>View <select bind:value={view}><option value="tree">Tree</option><option value="priority">Priority</option></select></label>
      <label>Review status <select bind:value={review}><option value="all">All</option><option value="unreviewed">Unreviewed</option><option value="reviewed-current">Reviewed</option><option value="changed-since-reviewed">Changed since review</option></select></label>
      <label><input type="checkbox" bind:checked={showIgnored} /> Show ignored</label>
    </div>
  {/if}
  <ul class="rows">
    {#each rows as row (row.kind === 'directory' ? `d:${row.path}` : `f:${row.entry.file.id}`)}
      {#if row.kind === 'directory'}
        <li class="row" style={`--depth: ${row.depth}`}>
          <button class="main" type="button" aria-label={`${collapsed.has(row.path) ? 'Expand' : 'Collapse'} folder ${row.path}`} aria-expanded={!collapsed.has(row.path)} onclick={() => toggleFolder(row.path)}><span class="chevron" aria-hidden="true">{collapsed.has(row.path) ? '▸' : '▾'}</span><svg aria-hidden="true" width="15" height="14" viewBox="0 0 16 14" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M1 3V1h5l2 2h7v10H1z" /></svg><span class="name" title={row.path}>{row.name}</span><small>{row.count}</small></button>
          <button class="more" type="button" aria-label={`Actions for folder ${row.path}`} aria-expanded={menu === `d:${row.path}`} onclick={(event) => toggleMenu(`d:${row.path}`, event.currentTarget)}>⋯</button>
          {#if menu === `d:${row.path}`}
            <div class="menu" aria-label={`Scope actions for folder ${row.path}`}>
              {#each ['include', 'exclude'] as action}{#each ['review', 'repository'] as lifetime}
                <button type="button" disabled={pending} onclick={() => scope('directory', row.path, action as 'include' | 'exclude', lifetime as 'review' | 'repository')}>{action === 'include' ? 'Include' : 'Exclude'} {lifetime === 'review' ? 'this review' : 'always'}</button>
              {/each}{/each}
            </div>
          {/if}
        </li>
      {:else}
        {@const file = row.entry.file}
        {@const ignored = row.entry.state?.scope.ignored ?? false}
        <li class="row" class:selected={selectedId === file.id} class:ignored style={`--depth: ${row.depth}`}>
          <button class="main" type="button" aria-current={selectedId === file.id ? 'true' : undefined} aria-label={`Open ${file.path}${exclusionDescription(row.entry.state)}`} title={`${file.path}${exclusionDescription(row.entry.state)}`} onclick={() => onSelect(file.id)}><span class="chevron" aria-hidden="true"></span><span class="icon" class:code={['.ts', '.tsx', '.js', '.jsx', '.rs', '.py', '.go'].includes(extensionOf(file.path))} aria-hidden="true"><svg width="14" height="16" viewBox="0 0 14 16" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M2 1h6l4 4v10H2z M8 1v4h4"/>{#if ['.ts', '.tsx', '.js', '.jsx', '.rs', '.py', '.go'].includes(extensionOf(file.path))}<path d="m6 8-2 2 2 2m2-4 2 2-2 2"/>{:else}<path d="M4 9h5M4 12h4"/>{/if}</svg></span><span class="name">{view === 'priority' ? file.path : file.path.split('/').at(-1)}</span></button>
          <button class="more" type="button" aria-label={`Actions for file ${file.path}`} aria-expanded={menu === `f:${file.id}`} onclick={(event) => toggleMenu(`f:${file.id}`, event.currentTarget)}>⋯</button>
          {#if menu === `f:${file.id}`}
            <div class="menu" aria-label={`Scope actions for file ${file.path}`}>
              {#each ['include', 'exclude'] as action}{#each ['review', 'repository'] as lifetime}
                <button type="button" disabled={pending} onclick={() => scope('file', file.path, action as 'include' | 'exclude', lifetime as 'review' | 'repository')}>{action === 'include' ? 'Include' : 'Exclude'} {lifetime === 'review' ? 'this review' : 'always'}</button>
              {/each}{/each}
            </div>
          {/if}
        </li>
      {/if}
    {/each}
  </ul>
</nav>

<style>
  .navigator { position: relative; font-size: 13px; min-width: 0; color: inherit; }
  .toolbar { display: flex; align-items: center; gap: 4px; padding: 6px; }
  .toolbar input[type=search] { min-width: 0; flex: 1; border: 1px solid var(--color-line); border-radius: 4px; padding: 4px; background: var(--color-surface); color: inherit; }
  .regex { white-space: nowrap; font-size: 11px; }
  .filter-button, .more { border: 0; background: transparent; color: inherit; cursor: pointer; }
  .filter-button { border: 1px solid var(--color-line); border-radius: 4px; padding: 4px; }
  .popover, .menu { position: absolute; z-index: 10; right: 4px; background: var(--color-surface); color: var(--color-ink); border: 1px solid var(--color-line); border-radius: 5px; padding: 8px; box-shadow: 0 4px 12px #0004; }
  .popover select, .menu button { background: var(--color-surface); color: var(--color-ink); border-color: var(--color-line); }
  .popover { width: min(340px, 95%); display: grid; gap: 8px; max-height: 60vh; overflow: auto; }
  .popover fieldset { border: 0; padding: 0; margin: 0; }
  .popover legend { font-weight: 600; }
  .type-row { display: flex; justify-content: space-between; gap: 4px; padding: 3px 0; }
  .type-row select { max-width: 125px; }
  .rows { list-style: none; padding: 0; margin: 0; overflow: visible; }
  .row { position: relative; display: flex; align-items: center; height: 28px; padding-left: calc(var(--depth) * 14px); }
  .row:hover, .row:focus-within { background: var(--color-surface); }
  .row.selected { background: var(--color-accent-soft); }
  .row.ignored { opacity: .6; }
  .main { display: flex; align-items: center; gap: 5px; min-width: 0; flex: 1; text-align: left; border: 0; background: transparent; color: inherit; cursor: pointer; height: 100%; }
  .chevron { width: 12px; flex: none; }
  .icon { flex: none; display: inline-flex; color: var(--color-muted); }
  .icon.code { color: var(--color-accent); }
  .name { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  small { color: var(--color-muted); }
  .more { opacity: .55; width: 26px; flex: none; }
  .row:hover .more, .row:focus-within .more { opacity: 1; }
  @media (hover: none) { .more { opacity: 1; width: 32px; } }
  .menu { top: 25px; display: grid; min-width: 150px; }
  .menu button { border: 0; background: transparent; padding: 5px; text-align: left; cursor: pointer; }
  .menu button:hover { background: var(--color-accent-soft); }
  button:focus-visible, input:focus-visible, select:focus-visible { outline: 2px solid var(--color-accent); outline-offset: 1px; }
  .error { color: var(--color-danger); margin: 4px 8px; }
</style>
