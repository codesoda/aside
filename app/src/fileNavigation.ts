import type { ChangedFile } from './generated/ChangedFile';
import type { FileState } from './generated/FileState';
import type { QueueView } from './generated/QueueView';

export type NavigationView = 'tree' | 'priority';
export type ReviewFilter = 'all' | 'unreviewed' | 'reviewed-current' | 'changed-since-reviewed';
export type NavigationFilters = { query: string; regex: boolean; extensions: string[]; view: NavigationView; review: ReviewFilter; showIgnored: boolean };
export type NavigationEntry = { file: ChangedFile; state?: FileState; priority?: number };
export type NavigationRow = { kind: 'directory'; path: string; name: string; depth: number; count: number } | { kind: 'file'; entry: NavigationEntry; depth: number };

/** Match the backend's basename categories exactly; preference keys are case-sensitive. */
export function extensionOf(path: string): string {
  const name = path.split('/').at(-1) ?? '';
  if (name.startsWith('.')) return 'dotfile';
  const dot = name.lastIndexOf('.');
  return dot > 0 && dot < name.length - 1 ? name.slice(dot) : 'no-extension';
}
export function extensionLabel(extension: string): string {
  return extension === 'dotfile' ? 'Dotfiles' : extension === 'no-extension' ? 'No extension' : extension;
}
export function extensionCounts(files: ChangedFile[]): { extension: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const file of files) {
    const ext = extensionOf(file.path);
    counts.set(ext, (counts.get(ext) ?? 0) + 1);
  }
  return [...counts].sort(([a], [b]) => a.localeCompare(b)).map(([extension, count]) => ({ extension, count }));
}

/** Bounded subset: character classes, anchors, escaped punctuation and one repetition.
 * No groups, alternation, backreferences or repeated repetitions. */
export function pathMatcher(query: string, regex: boolean): { matches: (path: string) => boolean; error: string | null } {
  if (!regex) {
    const needle = query.toLowerCase();
    return { matches: (path) => path.toLowerCase().includes(needle), error: null };
  }
  const unsupported = 'Regex supports up to 120 characters, escaped punctuation, and no groups, alternatives, or braces.';
  if (query.length > 120) return { matches: () => false, error: unsupported };
  let repetitions = 0;
  let inClass = false;
  for (let i = 0; i < query.length; i++) {
    const char = query[i];
    if (char === '\\') {
      if (i + 1 >= query.length || !/[.\\/\-_^$+*?\[\]]/.test(query[++i])) return { matches: () => false, error: unsupported };
    } else if (char === '[' && !inClass) inClass = true;
    else if (char === ']' && inClass) inClass = false;
    else if (!inClass && /[()|{}]/.test(char)) return { matches: () => false, error: unsupported };
    else if (!inClass && /[+*?]/.test(char)) repetitions++;
  }
  if (repetitions > 1) return { matches: () => false, error: 'Regex supports at most one repetition (+, * or ?).' };
  try {
    const pattern = new RegExp(query, 'i');
    return { matches: (path) => path.length <= 2048 && pattern.test(path), error: null };
  } catch { return { matches: () => false, error: 'Invalid regex pattern.' }; }
}

export function visibleEntries(files: ChangedFile[], states: FileState[], queue: QueueView | null, filters: NavigationFilters): { entries: NavigationEntry[]; error: string | null } {
  const matcher = pathMatcher(filters.query, filters.regex);
  const byId = new Map(states.map((state) => [state.fileId, state]));
  const priority = new Map(queue?.queue.items.map((item, index) => [item.fileId, index]) ?? []);
  const entries = files.filter((file) => {
    const state = byId.get(file.id);
    return matcher.matches(file.path) && filters.extensions.includes(extensionOf(file.path))
      && (filters.showIgnored || !state?.scope.ignored)
      && (filters.review === 'all' || state?.reviewStatus === filters.review);
  }).map((file) => ({ file, state: byId.get(file.id), priority: priority.get(file.id) }));
  if (filters.view === 'priority') entries.sort((a, b) => (a.priority ?? Infinity) - (b.priority ?? Infinity) || a.file.path.localeCompare(b.file.path));
  return { entries, error: matcher.error };
}

export function navigationRows(entries: NavigationEntry[], view: NavigationView, collapsed: Set<string>): NavigationRow[] {
  if (view === 'priority') return entries.map((entry) => ({ kind: 'file', entry, depth: 0 }));
  type Folder = { path: string; name: string; children: Map<string, Folder>; files: NavigationEntry[]; count: number };
  const root: Folder = { path: '', name: '', children: new Map(), files: [], count: 0 };
  for (const entry of entries) {
    let folder = root;
    folder.count++;
    for (const name of entry.file.path.split('/').slice(0, -1)) {
      const path = folder.path ? `${folder.path}/${name}` : name;
      if (!folder.children.has(name)) folder.children.set(name, { path, name, children: new Map(), files: [], count: 0 });
      folder = folder.children.get(name)!;
      folder.count++;
    }
    folder.files.push(entry);
  }
  const rows: NavigationRow[] = [];
  function visit(folder: Folder, depth: number) {
    for (const child of [...folder.children.values()].sort((a, b) => a.name.localeCompare(b.name))) {
      rows.push({ kind: 'directory', path: child.path, name: child.name, depth, count: child.count });
      if (!collapsed.has(child.path)) visit(child, depth + 1);
    }
    for (const entry of folder.files.sort((a, b) => a.file.path.localeCompare(b.file.path))) rows.push({ kind: 'file', entry, depth });
  }
  visit(root, 0);
  return rows;
}
