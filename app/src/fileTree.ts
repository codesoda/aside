import type { ChangedFile } from './generated/ChangedFile';
import type { FileState } from './generated/FileState';

export function nextUnreviewedFile(files: ChangedFile[], states: FileState[], currentId: string): ChangedFile | undefined {
  const currentIndex = files.findIndex((file) => file.id === currentId);
  if (currentIndex < 0) return undefined;
  const byId = new Map(states.map((state) => [state.fileId, state]));
  for (let step = 1; step < files.length; step++) {
    const candidate = files[(currentIndex + step) % files.length];
    const state = byId.get(candidate.id);
    if (state && !state.scope.ignored && state.reviewStatus !== 'reviewed-current') return candidate;
  }
  return undefined;
}


export type FileTreeNode = {
  name: string;
  path: string;
  folders: FileTreeNode[];
  files: ChangedFile[];
};

/** Keep directory identity by full path, not display name. */
export function buildFileTree(files: ChangedFile[]): FileTreeNode {
  const root: FileTreeNode = { name: '', path: '', folders: [], files: [] };
  for (const file of files) {
    const segments = file.path.split('/');
    let folder = root;
    for (const name of segments.slice(0, -1)) {
      let child = folder.folders.find((entry) => entry.name === name);
      if (!child) {
        child = { name, path: folder.path ? `${folder.path}/${name}` : name, folders: [], files: [] };
        folder.folders.push(child);
      }
      folder = child;
    }
    folder.files.push(file);
  }
  function sort(node: FileTreeNode) {
    node.folders.sort((a, b) => a.name.localeCompare(b.name));
    node.files.sort((a, b) => a.path.localeCompare(b.path));
    node.folders.forEach(sort);
  }
  sort(root);
  return root;
}
