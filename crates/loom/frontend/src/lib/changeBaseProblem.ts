import type { ChangeBase } from '../types';

/** The human reason a change-set or commit listing has no base to diff
 * against, so the two surfaces explain an unavailable base identically. */
export function changeBaseProblem(base: ChangeBase | null | undefined, noun: string): string {
  if (base?.state !== 'unavailable') return '';
  const reasons: Record<typeof base.reason, string> = {
    unborn_head: 'this worktree has no commits yet',
    missing_base: `no local or remote-tracking ref resolves the base ${base.reference}`,
    no_merge_base: `this branch shares no history with ${base.reference}`,
  };
  return `${noun} unavailable: ${reasons[base.reason]}.`;
}
