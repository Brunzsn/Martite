import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { generateQueries } from './generate-queries';
import { grammarDir, runTreeSitter } from './tree-sitter';

const repoRoot = resolve(grammarDir, '..', '..');
const zedDir = resolve(grammarDir, '..', 'zed-martite');

const queryFiles = [
  join(grammarDir, 'queries', 'highlights.scm'),
  join(grammarDir, 'queries', 'locals.scm'),
  join(grammarDir, 'queries', 'tags.scm'),
  ...(existsSync(zedDir)
    ? [
        join(zedDir, 'languages', 'martite', 'highlights.scm'),
        join(zedDir, 'languages', 'martite', 'brackets.scm'),
        join(zedDir, 'languages', 'martite', 'indents.scm'),
        join(zedDir, 'languages', 'martite', 'outline.scm'),
        join(zedDir, 'languages', 'martite', 'textobjects.scm'),
        join(zedDir, 'languages', 'martite', 'runnables.scm'),
        join(zedDir, 'languages', 'martite', 'overrides.scm'),
      ]
    : []),
];

const roots = [
  'examples',
  'crates/martite-conformance/fixtures',
  'crates/martite-fmt/tests/fixtures',
  'crates/martite-cli/tests/fixtures',
  'fuzz/corpus/checker',
];

function collect(dir: string, out: string[]): void {
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

if (!generateQueries(true)) {
  process.exit(1);
}

for (const query of queryFiles) {
  if (!existsSync(query)) {
    console.error(`Missing generated query: ${query}`);
    process.exit(1);
  }
}

const templates = join(zedDir, 'languages', 'martite', 'tasks.json');
if (existsSync(templates)) {
  const tasks = JSON.parse(readFileSync(templates, 'utf8')) as { label: string; tags?: string[] }[];
  for (const task of tasks) {
    if (!task.label || !Array.isArray(task.tags ?? [])) {
      console.error(`Invalid task entry: ${JSON.stringify(task)}`);
      process.exit(1);
    }
  }
}

const files: string[] = [];
for (const root of roots) {
  const path = join(repoRoot, root);
  if (existsSync(path) && statSync(path).isDirectory()) {
    collect(path, files);
  }
}
files.sort();

if (files.length === 0) {
  console.error('No fixtures found for query validation.');
  process.exit(1);
}

const tempDir = mkdtempSync(join(tmpdir(), 'martite-queries-'));
const pathsFile = join(tempDir, 'paths.txt');
writeFileSync(pathsFile, files.join('\n'));

let failed = false;
for (const query of queryFiles) {
  const result = runTreeSitter(['query', '-q', '--paths', pathsFile, query]);
  if (result.status !== 0) {
    failed = true;
    console.error(`Query failed: ${relative(repoRoot, query).replaceAll('\\', '/')}`);
    if (result.stderr.trim()) {
      console.error(result.stderr.trim());
    }
  }
}

rmSync(tempDir, { recursive: true, force: true });

if (failed) {
  process.exit(1);
}

console.log(`Validated ${queryFiles.length} query files against ${files.length} fixtures.`);
