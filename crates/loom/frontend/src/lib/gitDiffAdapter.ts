import type { ChangeFile, ChangeHunk, ChangeLine } from '../types';

const LINE_PREFIX: Record<ChangeLine['kind'], string> = {
  context: ' ',
  addition: '+',
  deletion: '-',
};

// `@@ -oldStart[,oldCount] +newStart[,newCount] @@ [section heading]`.
const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$/;

/** A count of 1 is spelled without its `,1` suffix in unified diffs. */
function countPart(start: string, count: number): string {
  return count === 1 ? start : `${start},${count}`;
}

/**
 * The backend drops lines past its bounds while keeping the hunk's original
 * `@@` header, whose counts then describe lines that are no longer there.
 * Rebuild the header from the lines that actually survived, reusing the
 * original starts and section heading; return null for a hunk left with no
 * lines at all.
 */
function hunkToPatchText(hunk: ChangeHunk): string | null {
  if (!hunk.lines.length) return null;
  const lines = hunk.lines.map((line) => `${LINE_PREFIX[line.kind]}${line.text}`);
  if (!hunk.truncated) return [hunk.header, ...lines].join('\n');
  const match = HUNK_HEADER.exec(hunk.header);
  if (!match) return [hunk.header, ...lines].join('\n');
  const oldCount = hunk.lines.filter((line) => line.old_line != null).length;
  const newCount = hunk.lines.filter((line) => line.new_line != null).length;
  const header = `@@ -${countPart(match[1]!, oldCount)} +${countPart(match[3]!, newCount)} @@${match[5] ?? ''}`;
  return [header, ...lines].join('\n');
}

export interface GitDiffViewData {
  oldFile?: { fileName: string; content?: string };
  newFile?: { fileName: string; content?: string };
  hunks: string[];
}

/**
 * Adapts a server-computed `ChangeFile` (already parsed into typed hunks and
 * lines) into the raw-unified-diff-per-file shape `@git-diff-view/vue`
 * expects. No diff parsing happens here — only text reassembly — since the
 * backend already did the parsing and truncation.
 */
export function toGitDiffViewData(file: ChangeFile): GitDiffViewData | null {
  if (file.content !== 'text') return null;
  const bodies = file.hunks.map(hunkToPatchText).filter((text): text is string => text != null);
  if (!bodies.length) return null;
  // An added or untracked file never had a base version: git spells its old
  // side `/dev/null`.
  const oldPath =
    file.status === 'added' || file.status === 'untracked' ? null : (file.old_path ?? file.path);
  const newPath = file.status === 'deleted' ? null : file.path;
  const oldName = oldPath ? oldPath.display : '/dev/null';
  const newName = newPath ? newPath.display : '/dev/null';
  const header = `--- ${oldPath ? `a/${oldName}` : oldName}\n+++ ${newPath ? `b/${newName}` : newName}`;
  // The server attaches whole-file content when it fits its bounds; that is
  // what lets the renderer show real lines behind its hunk expand controls.
  return {
    oldFile: oldPath
      ? { fileName: oldName, ...(file.old_content ? { content: file.old_content } : {}) }
      : undefined,
    newFile: newPath
      ? { fileName: newName, ...(file.new_content ? { content: file.new_content } : {}) }
      : undefined,
    hunks: [`${header}\n${bodies.join('\n')}`],
  };
}
