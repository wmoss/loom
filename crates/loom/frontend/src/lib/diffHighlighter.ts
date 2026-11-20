import {
  highlighter,
  type DiffAST,
  type DiffFileHighlighter,
  type SyntaxLine,
} from '@git-diff-view/vue';
import type { BundledLanguage, DiffHighlighter as WindowedEngine } from '@git-diff-view/shiki';
import type { ChangeFile, ChangeHunk, ChangeSide } from '../types';

/**
 * Windowed syntax highlighting for the diff view.
 *
 * The stock highlighter tokenizes the *whole* side — real content when the
 * bounds allowed it, else a line-number skeleton the renderer composes from
 * the hunks — and skips any side past a line-count cap. That made the
 * failures this module exists to remove: a file past the cap renders
 * silently un-highlighted, and a grammar that needs the block a hunk sits
 * inside (a Vue SFC's `<script>` region) mis-highlights the skeleton,
 * which never saw that block.
 *
 * Here the register-highlighter seam `@git-diff-view/vue` already provides
 * is used to highlight **hunk windows** instead: each hunk's own lines,
 * plus a margin of real lines above and below when the side's whole-file
 * text is known, cut from that side's text and tokenized as a fragment.
 * Cost follows the visible diff — a 50,000-line file highlights as fast
 * as a 50-line one — so there is no line-count cap to raise or hit, and
 * the fragment's margin gives block grammars the structure they need.
 * Lines outside every window render plain; a line whose remapped tokens
 * would not match the raw line exactly is dropped rather than rendered
 * wrong.
 *
 * Two engines can tokenize a window, in preference order: **Shiki**, with
 * VS Code's TextMate grammars and themes (loaded lazily on the first
 * diff this module serves; until then — and whenever a fragment's
 * language is not registered — fragments fall to the stock lowlight
 * engine, exactly as the renderer does with no custom one registered).
 * The engine that a highlighter will use is encoded in its name, so the
 * moment the Shiki engine settles, the panel's rebuilt highlighters carry
 * a new name — and the renderer re-runs its syntax pass for exactly that
 * reason, upgrading already-mounted windows in place.
 */
const CONTEXT_LINES = 40;

/** The languages the Shiki engine loads. The stock engine's set plus the
 * ones a loom review actually reads — this repo's own Rust, and the
 * manifests and configs around it. Everything else Shiki knows can still
 * be requested per-fragment, but these are pre-loaded. */
/** The languages the Shiki engine preloads. Kept deliberately small — the
 * engine compiles every grammar it loads before it is ready, so each
 * entry costs first-visit time for every viewer that opens a diff: what a
 * loom review actually reads, plus this repo's own Rust and the manifests
 * around it. Other languages fall to lowlight per-fragment. */
const SHIKI_LANGS: BundledLanguage[] = [
  'bash',
  'css',
  'diff',
  'go',
  'html',
  'javascript',
  'json',
  'jsx',
  'markdown',
  'python',
  'rust',
  'sql',
  'toml',
  'tsx',
  'typescript',
  'vue',
  'xml',
  'yaml',
];

/** The Shiki engine once its load settles — `null` until then, and forever
 * after a failed load (lowlight keeps serving the windows). */
let shikiEngine: WindowedEngine | null = null;
let shikiLoading: Promise<boolean> | null = null;

/** Starts the Shiki engine load (lazily, the first call) and resolves when
 * it settles: `true` when the engine is ready, `false` when the load
 * failed and lowlight remains. */
export function whenShikiReady(): Promise<boolean> {
  shikiLoading ??= import('@git-diff-view/shiki')
    .then(async (shiki) => {
      shikiEngine = await shiki.getDiffViewHighlighter(SHIKI_LANGS);
      return true;
    })
    .catch(() => {
      shikiEngine = null;
      return false;
    });
  return shikiLoading;
}

/** One absolute, inclusive, 1-based line range worth highlighting. */
type Window = { start: number; end: number };

/** What `getAST` hands `processAST`: the raw text the renderer holds for
 * the side, and the windows of it to tokenize. Opaque to everything else —
 * the two functions are a closed pair. */
type WindowedAst = {
  raw: string;
  fileName: string;
  lang?: string;
  theme?: 'light' | 'dark';
  windows: Window[];
};

/** The renderer's own raw-line semantics (`File.doRaw`): split on `\n`,
 * every line but the last carries its newline. */
function fullLine(lines: string[], n: number): string | undefined {
  const text = lines[n - 1];
  if (text === undefined) return undefined;
  return n < lines.length ? `${text}\n` : text;
}

/** Whether the raw's line `n` holds the hunk line's text — agreeing up to
 * the one newline the change DTO drops and the renderer's raw keeps. */
function textAt(lines: string[], n: number | null, text: string): boolean {
  if (n == null || n < 1 || n > lines.length) return false;
  const raw = lines[n - 1]!;
  return raw === text || raw === `${text}\n` || `${raw}\n` === text;
}

/** The line range each hunk occupies on one side: its context plus that
 * side's own change lines, whatever line numbers the hunk's entries name. */
function hunkWindows(hunks: ChangeHunk[], side: ChangeSide): Window[] {
  const windows: Window[] = [];
  for (const hunk of hunks) {
    let start = Number.POSITIVE_INFINITY;
    let end = 0;
    for (const line of hunk.lines) {
      const n = side === 'old' ? line.old_line : line.new_line;
      if (n == null) continue;
      if (n < start) start = n;
      if (n > end) end = n;
    }
    if (end > 0) windows.push({ start, end });
  }
  return windows;
}

/** Expands each window by `expand` lines of real content and merges the
 * overlaps that creates, so two nearby hunks share one fragment. */
function mergeWindows(windows: Window[], expand: number, maxLine: number): Window[] {
  const merged: Window[] = [];
  for (const window of [...windows].sort((a, b) => a.start - b.start)) {
    const start = Math.max(1, window.start - expand);
    const end = Math.min(maxLine, window.end + expand);
    const last = merged[merged.length - 1];
    if (last && start <= last.end + 1) last.end = Math.max(last.end, end);
    else merged.push({ start, end });
  }
  return merged;
}

/** Which side a raw text is: exactly the old or new whole-file content when
 * the bounds shipped it, else — the skeleton the renderer composed from the
 * hunks, whose hunk lines sit at their real numbers while the gaps hold
 * placeholders — decided by which side's hunk lines the raw agrees with
 * more. A tie is not guessed: no side, no highlight. */
function detectSide(
  raw: string,
  lines: string[],
  hunks: ChangeHunk[],
  contents: { old: string | null; new: string | null },
): { side: ChangeSide | null; fromContent: boolean } {
  if (contents.old != null && raw === contents.old) return { side: 'old', fromContent: true };
  if (contents.new != null && raw === contents.new) return { side: 'new', fromContent: true };
  let oldScore = 0;
  let newScore = 0;
  for (const hunk of hunks) {
    for (const line of hunk.lines) {
      if (textAt(lines, line.old_line, line.text)) oldScore += 1;
      if (textAt(lines, line.new_line, line.text)) newScore += 1;
    }
  }
  if (oldScore > newScore) return { side: 'old', fromContent: false };
  if (newScore > oldScore) return { side: 'new', fromContent: false };
  return { side: null, fromContent: false };
}

/**
 * Builds the diff view's per-file syntax highlighter: one that tokenizes
 * hunk windows cut from the side's real text (the panel folds lazily-read
 * whole-file text into `file` before rendering) instead of the whole side.
 * The name carries four things the renderer's caches key re-arming on.
 * The snapshot scope comes first: the renderer keeps a global LRU of
 * tokenized `File` results, and a cached one remembers the name it was
 * last highlighted with — a new snapshot that reuses the old name lets
 * that cache answer "already done" for content it never saw, and the view
 * mounts dark (an in-place refresh to a new version is exactly that
 * shape). Then the file's identity (so the cache can never serve one
 * file's windows to another with the same content), whether whole-file
 * text is in hand — a lazy read landing mid-view changes the name, which
 * is exactly the signal the renderer uses to re-run this pass around the
 * new content instead of its skeleton — and the engine the panel expects
 * to tokenize with, so the Shiki engine settling re-arms every mounted
 * window the same way.
 *
 * `type` stays "class": the Shiki engine's dual-theme output embeds
 * CSS variables, not theme colors, so a theme switch needs no re-parse —
 * and "class" is also what keeps the renderer from re-arming its syntax
 * pass on every repaint.
 */
export function createWindowedHighlighter(
  file: Pick<ChangeFile, 'path' | 'hunks' | 'old_content' | 'new_content'>,
  engine: 'shiki' | 'lowlight',
  scope: string,
): DiffFileHighlighter {
  const withContent = file.old_content != null || file.new_content != null;
  return {
    name: `loom-windowed:${scope}:${file.path.bytes}:${withContent ? 'full' : 'hunks'}:${engine}`,
    type: 'class',
    maxLineToIgnoreSyntax: Number.POSITIVE_INFINITY,
    setMaxLineToIgnoreSyntax: () => undefined,
    ignoreSyntaxHighlightList: [],
    setIgnoreSyntaxHighlightList: () => undefined,
    // Never declines a language: the core checks this before consulting the
    // cap, and on a decline it would fall back to the stock highlighter —
    // whose own whole-file cap we just bypassed — running a full-file
    // auto-detect pass on the main thread. Instead every language stays
    // ours, and unsupported ones are handled below where the fragment
    // falls to the lowlight engine's own auto-detection, window-sized.
    hasRegisteredCurrentLang: () => true,
    getAST(raw, fileName, lang, theme) {
      const lines = raw.split('\n');
      const { side, fromContent } = detectSide(raw, lines, file.hunks, {
        old: file.old_content ?? null,
        new: file.new_content ?? null,
      });
      // A skeleton's gaps hold placeholder lines, so only real content may
      // grow a window's margin — the hunk's own lines are all it truly has.
      const windows = side
        ? mergeWindows(hunkWindows(file.hunks, side), fromContent ? CONTEXT_LINES : 0, lines.length)
        : [];
      return { raw, fileName, lang, theme, windows } as unknown as DiffAST;
    },
    processAST(ast) {
      const { raw, fileName, lang, theme, windows } = ast as unknown as WindowedAst;
      const lines = raw.split('\n');
      const syntaxFileObject: Record<number, SyntaxLine> = {};
      for (const window of windows) {
        const fragment = lines.slice(window.start - 1, window.end).join('\n');
        // Shiki serves the fragment when its engine has settled and it has
        // a grammar for the language; anything else — engine not loaded
        // (or failed), language unknown to it — falls to the lowlight
        // engine's own auto-detection, window-sized. The AST and its
        // walker always come from the same engine: their shapes differ.
        let fragmentLines: Record<number, SyntaxLine> | undefined;
        if (shikiEngine && lang && shikiEngine.hasRegisteredCurrentLang(lang)) {
          const shikiAst = shikiEngine.getAST(fragment, fileName, lang as never, theme);
          if (shikiAst) {
            fragmentLines = shikiEngine.processAST(shikiAst).syntaxFileObject;
          }
        }
        if (!fragmentLines) {
          const lowlightAst = highlighter.getAST(fragment, fileName, lang as never, theme);
          if (lowlightAst) {
            fragmentLines = highlighter.processAST(lowlightAst).syntaxFileObject;
          }
        }
        if (!fragmentLines) continue;
        for (let absolute = window.start; absolute <= window.end; absolute++) {
          const fragmentLine = fragmentLines[absolute - window.start + 1];
          if (!fragmentLine) continue;
          const remapped: SyntaxLine = {
            ...fragmentLine,
            lineNumber: absolute,
            nodeList: fragmentLine.nodeList.map(({ node, wrapper }) => ({
              node: { ...node, lineNumber: absolute },
              wrapper,
            })),
          };
          // Install only a line whose tokens spell the renderer's raw line
          // exactly. The one line that can drift is the fragment's last,
          // which loses the trailing newline its cut dropped — restore it
          // when the real file continues past the window; anything else is
          // a mapping bug, and renders plain instead of wrong.
          const expected = fullLine(lines, absolute);
          if (remapped.value !== expected) {
            if (absolute === window.end && `${remapped.value}\n` === expected) {
              remapped.value += '\n';
              const last = remapped.nodeList[remapped.nodeList.length - 1];
              if (last) last.node = { ...last.node, value: `${last.node.value}\n` };
            } else {
              continue;
            }
          }
          syntaxFileObject[absolute] = remapped;
        }
      }
      return { syntaxFileObject, syntaxFileLineNumber: lines.length };
    },
  };
}
