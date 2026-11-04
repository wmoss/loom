import test from 'node:test';
import assert from 'node:assert/strict';
import { buildChangeFileTree } from './changeFileTree.ts';

// The order the server serves (and the review pane renders): raw path bytes,
// ascending. The reference sort works on UTF-8 bytes directly, so it stays
// honest where a JS code-unit sort would silently diverge (astral names).
const byUtf8 = (left, right) => {
  const a = Buffer.from(left, 'utf8');
  const b = Buffer.from(right, 'utf8');
  const shared = Math.min(a.length, b.length);
  for (let i = 0; i < shared; i++) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  }
  return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
};

const served = (displays) =>
  displays.map((display) => ({ display })).sort((a, b) => byUtf8(a.display, b.display));

const leavesOf = (nodes) => {
  const leaves = [];
  const walk = (level) => {
    for (const node of level) {
      if (node.folder) walk(node.folder);
      else leaves.push(node.path);
    }
  };
  walk(nodes);
  return leaves;
};

test('the tree walks its leaves in exactly the served file order', () => {
  // The cases that pull a "folders first, alphabetical (locale)" tree out of
  // sync with the flat list: a hyphenated sibling directory (loom-ctx before
  // loom/), mixed case (Zebra before apple by byte order), a folder
  // interleaved between same-prefix files (b.txt before b/x before b2), and
  // an astral name (U+1F389) whose UTF-8 lead byte sorts it ABOVE the
  // U+E000-range sibling its surrogate halves would put it below.
  const displays = [
    'Zebra.txt',
    'apple.txt',
    'a.txt',
    'b.txt',
    'b2.txt',
    'b/x.txt',
    '\uE3FF.txt',
    '\u{1F389}.txt',
    'crates/loom/frontend/a.vue',
    'crates/loom-ctx/src/b.rs',
  ];
  const files = served(displays);
  const tree = buildChangeFileTree(files, (file) => file.display);
  // Depth-first leaf order is the input order — the rail cannot drift from
  // the pane because they read the same sequence.
  assert.deepEqual(
    leavesOf(tree),
    files.map((file) => file.display),
  );
  // The astral ordering the bytes dictate, against a reference that cannot
  // share the tree's comparator by construction: the private-use name sorts
  // below the emoji one, where a code-unit sort would invert them.
  assert.deepEqual(leavesOf(tree).slice(-2), ['\uE3FF.txt', '\u{1F389}.txt']);
});

test('siblings interleave folders and files by path order, not folders first', () => {
  const tree = buildChangeFileTree(served(['a.txt', 'b/x.txt', 'b2.txt']), (file) => file.display);
  // Root children: file a.txt, folder b, file b2 — b sits between them,
  // because 'b/' sorts after 'a.txt' and before 'b2'.
  assert.deepEqual(
    tree.map((node) => (node.folder ? `${node.name}/` : node.name)),
    ['a.txt', 'b/', 'b2.txt'],
  );
});

test('a directory name compares as name + slash against sibling files', () => {
  // The loom repo's own layout: byte order puts loom-ctx before loom/, so
  // the rail must too — an alphabetical folders-first tree inverts them.
  const tree = buildChangeFileTree(
    served(['crates/loom/frontend/a.vue', 'crates/loom-ctx/src/b.rs']),
    (file) => file.display,
  );
  const crates = tree[0];
  assert.equal(crates.name, 'crates');
  assert.deepEqual(
    crates.folder.map((node) => node.name),
    ['loom-ctx', 'loom'],
  );
  // And the leaf order still matches the flat sort.
  assert.deepEqual(leavesOf(tree), ['crates/loom-ctx/src/b.rs', 'crates/loom/frontend/a.vue']);
});
