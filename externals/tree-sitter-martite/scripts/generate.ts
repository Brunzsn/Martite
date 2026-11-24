import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { checkGrammarJs, writeGrammarJs } from './generate-grammar';
import { grammarDir, runTreeSitter } from './tree-sitter';

const check = process.argv.includes('--check');

if (check) {
  if (!checkGrammarJs()) {
    process.exit(1);
  }
} else {
  writeGrammarJs();
}

const outputDir = check ? mkdtempSync(join(tmpdir(), 'martite-grammar-')) : join(grammarDir, 'src');
const generate = runTreeSitter([
  'generate',
  '--abi',
  '15',
  '--js-runtime',
  'bun',
  '--output',
  outputDir,
]);

if (generate.status !== 0) {
  if (check) {
    rmSync(outputDir, { recursive: true, force: true });
  }
  console.error(generate.stderr || generate.stdout);
  process.exit(1);
}

if (check) {
  const candidates = [
    'parser.c',
    'grammar.json',
    'node-types.json',
    'tree_sitter/alloc.h',
    'tree_sitter/array.h',
    'tree_sitter/parser.h',
  ];
  const stale: string[] = [];
  for (const relative of candidates) {
    const generated = join(outputDir, relative);
    const current = join(grammarDir, 'src', relative);
    if (!existsSync(generated)) {
      continue;
    }
    if (!existsSync(current) || readFileSync(current).compare(readFileSync(generated)) !== 0) {
      stale.push(`src/${relative}`);
    }
  }
  rmSync(outputDir, { recursive: true, force: true });
  if (stale.length > 0) {
    console.error('Generated parser files are stale. Run `bun run generate` and commit src/.');
    for (const path of stale) {
      console.error(`  ${path}`);
    }
    process.exit(1);
  }
}

const queries = Bun.spawnSync(
  [process.execPath, join(grammarDir, 'scripts', 'generate-queries.ts'), ...(check ? ['--check'] : [])],
  {
    cwd: grammarDir,
    stdout: 'inherit',
    stderr: 'inherit',
  },
);

if (queries.exitCode !== 0) {
  process.exit(queries.exitCode ?? 1);
}
