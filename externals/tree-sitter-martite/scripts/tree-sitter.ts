import { existsSync } from 'node:fs';
import { delimiter, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const grammarDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');

export const cliDir = join(grammarDir, 'node_modules', 'tree-sitter-cli');
export const treeSitterBinary = join(
  cliDir,
  process.platform === 'win32' ? 'tree-sitter.exe' : 'tree-sitter',
);

export function requireTreeSitter(): string {
  if (!existsSync(treeSitterBinary)) {
    throw new Error(
      `tree-sitter CLI not found at ${treeSitterBinary}. Run \`bun run setup:cli\` first.`,
    );
  }
  return treeSitterBinary;
}

export interface RunOptions {
  cwd?: string;
}

export function runTreeSitter(
  args: string[],
  options: RunOptions = {},
): { status: number; stdout: string; stderr: string } {
  // Tree-sitter only recognizes the literal runtime names `bun`, `node`, and
  // `deno`; a path is treated as an unknown runtime. Add Bun's own directory to
  // PATH so `bun` resolves to the real executable instead of an npm shim.
  const pathKey = Object.keys(process.env).find((key) => key.toUpperCase() === 'PATH') ?? 'PATH';
  const inheritedPath = process.env[pathKey] ?? '';
  const bunDir = dirname(process.execPath);

  const result = Bun.spawnSync([requireTreeSitter(), ...args], {
    cwd: options.cwd ?? grammarDir,
    env: {
      ...process.env,
      [pathKey]: inheritedPath ? `${bunDir}${delimiter}${inheritedPath}` : bunDir,
    },
    stdout: 'pipe',
    stderr: 'pipe',
  });

  return {
    status: result.exitCode ?? 1,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}
