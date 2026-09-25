import test from 'node:test';
import assert from 'node:assert/strict';
import { toGitDiffViewData } from './gitDiffAdapter.ts';

function path(display) {
  return { bytes: Buffer.from(display).toString('base64url'), display };
}

function file(overrides) {
  return {
    status: 'modified',
    path: path('src/a.rs'),
    old_path: null,
    sources: ['unstaged'],
    additions: 1,
    deletions: 1,
    content: 'text',
    truncated: false,
    hunks: [
      {
        header: '@@ -2,2 +2,3 @@ fn main() {',
        truncated: false,
        lines: [
          { kind: 'context', old_line: 2, new_line: 2, text: 'same' },
          { kind: 'deletion', old_line: 3, new_line: null, text: 'old' },
          { kind: 'addition', old_line: null, new_line: 3, text: 'new' },
          { kind: 'addition', old_line: null, new_line: 4, text: 'next' },
        ],
      },
    ],
    ...overrides,
  };
}

test('reassembles a per-file unified diff from typed hunks', () => {
  const data = toGitDiffViewData(file({}));
  assert.deepEqual(data, {
    oldFile: { fileName: 'src/a.rs' },
    newFile: { fileName: 'src/a.rs' },
    hunks: [
      '--- a/src/a.rs\n+++ b/src/a.rs\n@@ -2,2 +2,3 @@ fn main() {\n same\n-old\n+new\n+next',
    ],
  });
});

test('an added file has no old side', () => {
  const data = toGitDiffViewData(file({ status: 'added' }));
  assert.equal(data.oldFile, undefined);
  assert.match(data.hunks[0], /^--- \/dev\/null\n\+\+\+ b\/src\/a\.rs\n/);
});

test('a deleted file has no new side', () => {
  const data = toGitDiffViewData(file({ status: 'deleted' }));
  assert.equal(data.newFile, undefined);
  assert.match(data.hunks[0], /\n\+\+\+ \/dev\/null\n/);
});

test('a rename carries the old path on the old side', () => {
  const data = toGitDiffViewData(file({ status: 'renamed', old_path: path('src/old.rs') }));
  assert.deepEqual(data.oldFile, { fileName: 'src/old.rs' });
  assert.deepEqual(data.newFile, { fileName: 'src/a.rs' });
});

test('non-text content is not adapted', () => {
  assert.equal(toGitDiffViewData(file({ content: 'binary' })), null);
});

test('a file with no hunks is not adapted', () => {
  assert.equal(toGitDiffViewData(file({ hunks: [] })), null);
});
