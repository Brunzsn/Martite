import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const grammarDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = resolve(grammarDir, '..', '..');

const roots = [
  'examples',
  'crates/martite-conformance/fixtures',
  'crates/martite-fmt/tests/fixtures',
  'crates/martite-cli/tests/fixtures',
  'fuzz/corpus/checker',
];

// These fixtures deliberately contain syntax errors and are used to exercise
// the parser's recovery paths.
const expectedErrorFixtures = new Set([
  'crates/martite-conformance/fixtures/bench/parser/malformed.mt',
  'fuzz/corpus/checker/deep-nesting.mt',
  'fuzz/corpus/checker/syntax-error.mt',
]);

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

function collect(dir, out) {
  if (!existsSync(dir)) {
    return;
  }
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === 'target') {
      continue;
    }
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      collect(path, out);
    } else if (entry.name.endsWith('.mt') || entry.name.endsWith('.mti')) {
      out.push(path);
    }
  }
}

const files = [];
for (const root of roots) {
  const path = join(repoRoot, root);
  if (existsSync(path) && statSync(path).isDirectory()) {
    collect(path, files);
  }
}
files.sort();

if (files.length === 0) {
  console.error('No .mt or .mti fixtures found.');
  process.exit(1);
}

const tempDir = mkdtempSync(join(tmpdir(), 'martite-treesitter-'));
const pathsFile = join(tempDir, 'paths.txt');
writeFileSync(pathsFile, files.join('\n'));

const result = spawnSync(
  binary,
  ['parse', '--json-summary', '--quiet', '--paths', pathsFile],
  { cwd: grammarDir, encoding: 'utf8' },
);

rmSync(tempDir, { recursive: true, force: true });

let summary;
try {
  const jsonStart = result.stdout.indexOf('{');
  summary = JSON.parse(result.stdout.slice(jsonStart));
} catch {
  console.error('Could not read tree-sitter parse summary.');
  console.error(`status=${result.status} stdout=${result.stdout.length} bytes files=${files.length}`);
  const debugPath = join(tmpdir(), 'martite-treesitter-summary.txt');
  writeFileSync(debugPath, result.stdout);
  console.error(`stdout saved to ${debugPath}`);
  if (result.stderr) {
    console.error(result.stderr.trim());
  }
  process.exit(1);
}

const byFile = new Map(
  (summary.parse_summaries ?? []).map((entry) => [resolve(entry.file), entry]),
);

const failures = [];
let validCount = 0;
let expectedErrorCount = 0;

for (const file of files) {
  const rel = relative(repoRoot, file).replaceAll('\\', '/');
  const entry = byFile.get(resolve(file));
  if (!entry) {
    failures.push(`${rel}: no parse summary`);
    continue;
  }

  if (expectedErrorFixtures.has(rel)) {
    expectedErrorCount++;
    if (entry.successful) {
      failures.push(`${rel}: expected syntax errors, but the parse succeeded`);
    }
    continue;
  }

  validCount++;
  if (!entry.successful) {
    failures.push(`${rel}: parse produced ERROR/MISSING nodes`);
  }
}

if (failures.length > 0) {
  console.error(`Fixture check failed for ${failures.length} file(s):`);
  for (const failure of failures) {
    console.error(`  ${failure}`);
  }
  process.exit(1);
}

console.log(
  `Parsed ${validCount} valid .mt/.mti fixtures without errors and confirmed ${expectedErrorCount} expected-error fixture.`,
);
