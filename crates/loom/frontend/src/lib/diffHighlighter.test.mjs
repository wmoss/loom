import test from 'node:test';
import assert from 'node:assert/strict';
import { createWindowedHighlighter, whenShikiReady } from './diffHighlighter.ts';

// The tests speak the renderer's own semantics: a side's raw is split on
// `\n` and every line but the last carries its newline, and a change file's
// `text` is the line without its newline.

const context = (old_line, new_line, text) => ({ kind: 'context', old_line, new_line, text });
const addition = (new_line, text) => ({ kind: 'addition', old_line: null, new_line, text });
const deletion = (old_line, text) => ({ kind: 'deletion', old_line, new_line: null, text });

function codeFile({ hunks, old_content = null, new_content = null, bytes = 'b3JnL2NvZGUuanM=' }) {
  return {
    path: { bytes, display: 'org/code.js' },
    hunks: hunks.map((lines) => ({ header: '@@ -1,6 +1,6 @@', lines, truncated: false })),
    old_content,
    new_content,
  };
}

function linesOf(count, maker = (n) => `const filler${n} = ${n};`) {
  return Array.from({ length: count }, (_, index) => maker(index + 1));
}

test('the hunk window is highlighted on its own side, with real-text margins', () => {
  const oldLines = linesOf(200);
  const newLines = oldLines.toSpliced(149, 1, 'const edited = "new";');
  const file = codeFile({
    hunks: [
      [
        context(147, 147, 'const filler147 = 147;'),
        context(148, 148, 'const filler148 = 148;'),
        context(149, 149, 'const filler149 = 149;'),
        deletion(150, 'const filler150 = 150;'),
        addition(150, 'const edited = "new";'),
        context(151, 151, 'const filler151 = 151;'),
      ],
    ],
    old_content: `${oldLines.join('\n')}`,
    new_content: `${newLines.join('\n')}`,
  });
  const highlight = createWindowedHighlighter(file, 'lowlight', 'changes-v1:test-scope');

  assert.equal(highlight.maxLineToIgnoreSyntax, Number.POSITIVE_INFINITY);
  assert.equal(highlight.type, 'class');
  // The scope rides first: the renderer's tokenized-file cache answers
  // "already highlighted" by name, so a new snapshot must never reuse the
  // previous version's name.
  assert.equal(
    highlight.name,
    `loom-windowed:changes-v1:test-scope:${file.path.bytes}:full:lowlight`,
  );

  const oldAst = highlight.getAST(file.old_content, 'org/code.js', undefined, 'light');
  const { syntaxFileObject: oldSyntax } = highlight.processAST(oldAst);
  // The whole window carries tokens — the hunk lines themselves…
  assert.ok(oldSyntax[150], 'the changed line is highlighted');
  assert.equal(oldSyntax[150].lineNumber, 150);
  assert.equal(oldSyntax[150].value, 'const filler150 = 150;\n');
  // …and the 40-line margin of real content above and below, whose values
  // are exactly the renderer's raw lines, trailing newlines included.
  assert.ok(oldSyntax[110], 'the margin above carries tokens');
  assert.equal(oldSyntax[110].value, 'const filler110 = 110;\n');
  assert.ok(oldSyntax[190], 'the margin below carries tokens');
  assert.equal(oldSyntax[190].value, 'const filler190 = 190;\n');
  // Beyond the window: plain rendering, no entry.
  assert.equal(oldSyntax[50], undefined);
  // The whole side is accounted for, not just its windows.
  const { syntaxFileLineNumber } = highlight.processAST(oldAst);
  assert.equal(syntaxFileLineNumber, 200);

  // The new side highlights by its own numbering: same edit, its own line.
  const newAst = highlight.getAST(file.new_content, 'org/code.js', undefined, 'light');
  const { syntaxFileObject: newSyntax } = highlight.processAST(newAst);
  assert.ok(newSyntax[150]);
  assert.equal(newSyntax[150].value, 'const edited = "new";\n');
});

test('a skeleton-composed raw keeps hunk lines highlighted at their real numbers', () => {
  // No whole-file content: the renderer composes a raw whose hunk lines sit
  // at their real line numbers while the gaps hold placeholder newlines.
  const hunks = [
    [
      context(3, 3, 'const three = 3;'),
      deletion(4, 'const gone = 4;'),
      addition(4, 'const fresh = 4;'),
      context(5, 5, 'const five = 5;'),
    ],
  ];
  const file = codeFile({ hunks });
  const skeleton = (side) =>
    Array.from({ length: 6 }, (_, index) => {
      const number = index + 1;
      const line = hunks[0].find(
        (entry) => (side === 'old' ? entry.old_line : entry.new_line) === number,
      );
      return line ? line.text : '';
    }).join('\n');

  const oldResult = highlightThrough(file, skeleton('old'));
  const oldHighlight = oldResult.syntaxFileObject;
  assert.ok(oldHighlight[4], 'the deleted line keeps its tokens');
  assert.equal(oldHighlight[4].value, 'const gone = 4;\n');
  assert.equal(oldHighlight[4].lineNumber, 4);
  // The skeleton's placeholder gaps carry nothing — but the hunk's own
  // context lines do, at their real numbers.
  assert.ok(oldHighlight[3]);
  assert.equal(oldHighlight[3].value, 'const three = 3;\n');

  const newHighlight = highlightThrough(file, skeleton('new')).syntaxFileObject;
  assert.ok(newHighlight[4], 'the added line keeps its tokens');
  assert.equal(newHighlight[4].value, 'const fresh = 4;\n');
  assert.equal(newHighlight[4].lineNumber, 4);
});

test('a raw no hunk line agrees with renders plain, never wrong', () => {
  const hunks = [[context(2, 2, 'const two = 2;'), deletion(3, 'const gone = 3;')]];
  const file = codeFile({ hunks });
  const { syntaxFileObject } = highlightThrough(
    file,
    'nothing agrees with any hunk line here\nor here',
  );
  assert.deepEqual(Object.keys(syntaxFileObject), []);
});

test('no line-count cap applies: a 12,000-line file highlights its hunk', () => {
  const oldLines = linesOf(12_000);
  const newLines = oldLines.toSpliced(11_998, 1, 'const edited = "new";');
  const file = codeFile({
    hunks: [
      [
        context(11_997, 11_997, 'const filler11997 = 11997;'),
        deletion(11_998, 'const filler11998 = 11998;'),
        addition(11_998, 'const edited = "new";'),
        context(11_999, 11_999, 'const filler11999 = 11999;'),
      ],
    ],
    old_content: `${oldLines.join('\n')}`,
    new_content: `${newLines.join('\n')}`,
  });
  const hl = createWindowedHighlighter(file, 'lowlight', 'changes-v1:test-scope');
  const ast = hl.getAST(file.old_content, 'org/code.js', undefined, 'light');
  const { syntaxFileObject } = hl.processAST(ast);
  // The stock highlighter skips any side past 2,000 lines; a windowed one
  // has no cap to consult — the hunk's own lines highlight regardless.
  assert.ok(syntaxFileObject[11_998]);
  assert.equal(syntaxFileObject[11_998].value, 'const filler11998 = 11998;\n');
});

test('the Shiki engine tokenizes windows once its load settles', async () => {
  const ready = await whenShikiReady();
  // The engine loads lazily and may genuinely fail to (a broken import, an
  // environment without its regex engine) — in that case the windows keep
  // serving through lowlight, which the test above covers.
  if (!ready) return;
  const oldLines = ['// a comment above', 'fn the_edit() {}', '// a comment below'];
  const newLines = ['// a comment above', 'fn edited_now() {}', '// a comment below'];
  const file = codeFile({
    hunks: [
      [
        context(1, 1, '// a comment above'),
        deletion(2, 'fn the_edit() {}'),
        addition(2, 'fn edited_now() {}'),
        context(3, 3, '// a comment below'),
      ],
    ],
    old_content: `${oldLines.join('\n')}`,
    new_content: `${newLines.join('\n')}`,
  });
  const hl = createWindowedHighlighter(file, 'shiki', 'changes-v1:test-scope');
  assert.equal(hl.name, `loom-windowed:changes-v1:test-scope:${file.path.bytes}:full:shiki`);
  const ast = hl.getAST(file.old_content, 'org/code.rs', 'rs', 'light');
  const { syntaxFileObject, syntaxFileLineNumber } = hl.processAST(ast);
  assert.equal(syntaxFileLineNumber, 3);
  assert.ok(syntaxFileObject[2], 'the edited line carries Shiki tokens');
  assert.equal(syntaxFileObject[2].value, 'fn the_edit() {}\n');
  assert.equal(syntaxFileObject[2].lineNumber, 2);
  // Shiki's dual-theme spans carry the CSS-variable colors the renderer's
  // own stylesheet keys on — not lowlight's class-based output. (Shiki also
  // emits an empty text node wrapped in the root `code` element per line;
  // the styled tokens' wrappers are the ones that carry the variables.)
  const hasStyleWrapper = syntaxFileObject[2].nodeList.some(({ wrapper }) =>
    typeof wrapper?.properties?.style === 'string'
      ? wrapper.properties.style.includes('--diff-view-')
      : false,
  );
  assert.ok(hasStyleWrapper, 'the tokens carry the Shiki theme variables');
});

/** Runs one raw through the highlighter's whole pair. */
function highlightThrough(file, raw) {
  const hl = createWindowedHighlighter(file, 'lowlight', 'changes-v1:test-scope');
  const ast = hl.getAST(raw, 'org/code.js', undefined, 'light');
  return hl.processAST(ast);
}
