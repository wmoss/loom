import type { ChangeFile, ChangeHunk, ChangeLine } from '../types';

const LINE_PREFIX: Record<ChangeLine['kind'], string> = {
  context: ' ',
  addition: '+',
  deletion: '-',
};

function hunkToPatchText(hunk: ChangeHunk): string {
  const lines = hunk.lines.map((line) => `${LINE_PREFIX[line.kind]}${line.text}`);
  return [hunk.header, ...lines].join('\n');
}

export interface GitDiffViewData {
  oldFile?: { fileName: string };
  newFile?: { fileName: string };
  hunks: string[];
}

/**
 * Adapts a server-computed `ChangeFile` (already parsed into typed hunks and
 * lines) into the raw-unified-diff-per-file shape `@git-diff-view/vue`
 * expects. No diff parsing happens here — only text reassembly — since the
 * backend already did the parsing and truncation.
 */
export function toGitDiffViewData(file: ChangeFile): GitDiffViewData | null {
  if (file.content !== 'text' || !file.hunks.length) return null;
  const oldPath = file.status === 'added' ? null : (file.old_path ?? file.path);
  const newPath = file.status === 'deleted' ? null : file.path;
  const oldName = oldPath ? oldPath.display : '/dev/null';
  const newName = newPath ? newPath.display : '/dev/null';
  const header = `--- ${oldPath ? `a/${oldName}` : oldName}\n+++ ${newPath ? `b/${newName}` : newName}`;
  const body = file.hunks.map(hunkToPatchText).join('\n');
  return {
    oldFile: oldPath ? { fileName: oldName } : undefined,
    newFile: newPath ? { fileName: newName } : undefined,
    hunks: [`${header}\n${body}`],
  };
}
