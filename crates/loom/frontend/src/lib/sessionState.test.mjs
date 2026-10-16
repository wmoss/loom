import test from 'node:test';
import assert from 'node:assert/strict';
import {
  canSend,
  conversationState,
  effectiveAttention,
  messageOf,
  sessionDescendants,
} from './sessionState.ts';

test('an orphaned ACP runtime surfaces Loom recovery guidance', () => {
  const session = {
    status: 'orphaned',
    last_activity_at: '2026-08-18T20:00:00Z',
    branch: {
      description: 'Agent status from before the disconnect',
      tags: [
        {
          key: 'runtime',
          value: 'attention',
          note: 'Loom lost its connection. Select Adopt to reconnect.',
          set_by: 'loom',
          set_at: '2026-08-18T20:00:00Z',
        },
      ],
    },
  };

  assert.equal(messageOf(session), 'Loom lost its connection. Select Adopt to reconnect.');
  assert.deepEqual(effectiveAttention(session), {
    key: 'runtime',
    level: 'attention',
    by: 'loom',
    raisedBy: 'watch',
    note: 'Loom lost its connection. Select Adopt to reconnect.',
    stale: false,
  });
});

test('a handoff is presented as a paused lifecycle transition', () => {
  const session = {
    status: 'running',
    transition: {
      kind: 'handoff',
      step: 'Transferring context to codex',
      started_at: '2026-08-28T20:00:00Z',
    },
    branch: { tags: [] },
  };

  assert.equal(canSend(session), false);
  assert.deepEqual(conversationState(session), {
    glyph: '▶',
    label: 'Handing off — Transferring context to codex',
    tone: 'info',
  });
});

function summary(id, branchId, parentSessionId = null, parentId = null) {
  return {
    id,
    status: 'running',
    parent_id: parentId,
    parent_session_id: parentSessionId,
    branch: { id: branchId },
  };
}

test('descendants follow the delegation thread transitively', () => {
  const sessions = [
    summary('root', 'b-root'),
    summary('child-a', 'b-a', 'root'),
    summary('grandchild', 'b-gg', 'child-a'),
    summary('child-b', 'b-b', 'root'),
    summary('unrelated', 'b-other', 'elsewhere'),
  ];

  assert.deepEqual(
    sessionDescendants(sessions, 'root').map((session) => session.id),
    ['child-a', 'grandchild', 'child-b'],
  );
  assert.deepEqual(
    sessionDescendants(sessions, 'child-a').map((session) => session.id),
    ['grandchild'],
  );
  assert.deepEqual(sessionDescendants(sessions, 'unrelated'), []);
});

test('descendants fall back to the legacy branch-id parent when unambiguous', () => {
  const sessions = [summary('root', 'b-root'), summary('legacy-child', 'b-legacy', null, 'b-root')];

  assert.deepEqual(
    sessionDescendants(sessions, 'root').map((session) => session.id),
    ['legacy-child'],
  );
});

test('descendants survive a corrupt legacy ancestry cycle', () => {
  const sessions = [
    summary('root', 'b-root'),
    // A cycle detached from any root: b and c name each other's branches as
    // parents, so the walk must terminate rather than recurse forever.
    summary('a', 'b-a', null, 'b-c'),
    summary('c', 'b-c', null, 'b-a'),
  ];

  assert.deepEqual(sessionDescendants(sessions, 'root'), []);
  assert.deepEqual(
    sessionDescendants(sessions, 'a').map((session) => session.id),
    ['c'],
  );
});
