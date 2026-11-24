import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const grammarDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const binary = join(
  grammarDir,
  'node_modules',
  'tree-sitter-cli',
  process.platform === 'win32' ? 'tree-sitter.exe' : 'tree-sitter',
);

if (!existsSync(binary)) {
  console.error(`tree-sitter CLI not found at ${binary}; run \`pnpm install\` first.`);
  process.exit(1);
}

const outputDir = mkdtempSync(join(tmpdir(), 'martite-grammar-'));
const result = spawnSync(binary, ['generate', '--abi', '15', '--output', outputDir], {
  cwd: grammarDir,
  encoding: 'utf8',
});

if (result.status !== 0) {
  console.error(result.stderr || result.stdout);
  rmSync(outputDir, { recursive: true, force: true });
  process.exit(1);
}

const candidates = [
  'src/parser.c',
  'src/grammar.json',
  'src/node-types.json',
  'src/tree_sitter/alloc.h',
  'src/tree_sitter/array.h',
  'src/tree_sitter/parser.h',
];

const failures = [];
for (const relative of candidates) {
  const current = join(grammarDir, relative);
  const generatedInOutput = join(outputDir, relative);
  const generatedFlat = join(outputDir, relative.replace(/^src\//, ''));
  const generated = existsSync(generatedInOutput) ? generatedInOutput : generatedFlat;

  if (!existsSync(generated)) {
    // Header files are not always emitted by every CLI mode; only compare the
    // files that the generator actually produced.
    continue;
  }

  if (!existsSync(current)) {
    failures.push(`${relative}: missing from src/`);
    continue;
  }

  if (readFileSync(current).compare(readFileSync(generated)) !== 0) {
    failures.push(`${relative}: differs from a fresh generate`);
  }
}

rmSync(outputDir, { recursive: true, force: true });

if (failures.length > 0) {
  console.error('Generated parser files are stale. Run `pnpm run generate` and commit src/.');
  for (const failure of failures) {
    console.error(`  ${failure}`);
  }
  process.exit(1);
}
