/** A file (or folder) in the change set's directory tree. */
export interface FileTreeNode<T> {
  name: string;
  /** The full path: the segment chain down to this node. */
  path: string;
  folder?: FileTreeNode<T>[];
  file?: T;
}

/**
 * Folds a change set's files into a directory tree whose depth-first leaf
 * order is exactly the order the files arrive in — which is the order the
 * review pane renders them in, the server's path-byte sort.
 *
 * That identity is the whole point: the file rail must list the same files
 * in the same order as the diff pane below it. It constrains how each level
 * sorts, because the flat sort interleaves rather than groups:
 *
 * - Children sort together — folders are not hoisted before files.
 * - A folder's name compares as `name + '/'`, the separator a flat path
 *   comparison would see next: `loom-ctx` sorts *before* `loom/` (a hyphen
 *   sorts before the slash), `b.txt` before `b/x`.
 * - The comparison is over the names' UTF-8 bytes — the exact bytes the
 *   server's path sort compares — never code units or `localeCompare`, both
 *   of which reorder what the bytes keep in place: `Zebra.txt` before
 *   `apple.txt`, and an astral name above (not below) a U+E000–U+FFFF
 *   sibling, as its encoding dictates.
 */
export function buildChangeFileTree<T>(
  files: readonly T[],
  pathOf: (file: T) => string,
): FileTreeNode<T>[] {
  const root: FileTreeNode<T>[] = [];
  const folders = new Map<string, FileTreeNode<T>>();
  for (const file of files) {
    const display = pathOf(file);
    const parts = display.split('/').filter(Boolean);
    const name = parts.pop() ?? display;
    let children = root;
    let prefix = '';
    for (const part of parts) {
      prefix = prefix ? `${prefix}/${part}` : part;
      let node = folders.get(prefix);
      if (!node) {
        node = { name: part, path: prefix, folder: [] };
        folders.set(prefix, node);
        children.push(node);
      }
      children = node.folder!;
    }
    children.push({ name, path: display, file });
  }
  const sortLevel = (nodes: FileTreeNode<T>[]) => {
    nodes.sort((a, b) =>
      compareUtf8(a.folder ? `${a.name}/` : a.name, b.folder ? `${b.name}/` : b.name),
    );
    for (const node of nodes) sortLevel(node.folder ?? []);
  };
  sortLevel(root);
  return root;
}

const encoder = new TextEncoder();

/**
 * Compares names by their UTF-8 bytes — the exact order the server sorts
 * path bytes in. A plain code-unit comparison agrees everywhere except
 * astral characters: their surrogate halves sort below the U+E000–U+FFFF
 * range their UTF-8 lead bytes sit above, so an emoji-named file would
 * land on the wrong side of a private-use-area sibling and pull the rail
 * out of step with the pane.
 */
function compareUtf8(left: string, right: string): number {
  const a = encoder.encode(left);
  const b = encoder.encode(right);
  const shared = Math.min(a.length, b.length);
  for (let i = 0; i < shared; i++) {
    if (a[i] !== b[i]) return a[i]! < b[i]! ? -1 : 1;
  }
  return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
}
