import { chmodSync, existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { gunzipSync } from 'node:zlib';
import { cliDir, treeSitterBinary } from './tree-sitter';

const packageJson = JSON.parse(readFileSync(join(cliDir, 'package.json'), 'utf8')) as {
  version: string;
};
const version = packageJson.version;

if (existsSync(treeSitterBinary)) {
  const existing = Bun.spawnSync([treeSitterBinary, '--version'], {
    stdout: 'pipe',
    stderr: 'pipe',
  });
  if (existing.exitCode === 0 && existing.stdout.toString().includes(version)) {
    process.exit(0);
  }
}

const platformMatrix: Record<string, { name: string; arch: Record<string, string> }> = {
  darwin: { name: 'macos', arch: { arm64: 'arm64', x64: 'x64' } },
  linux: {
    name: 'linux',
    arch: { arm: 'arm', arm64: 'arm64', ppc64: 'powerpc64', x64: 'x64', x86: 'x86' },
  },
  win32: { name: 'windows', arch: { arm64: 'arm64', ia32: 'x86', x64: 'x64', x86: 'x86' } },
};

const platform = platformMatrix[process.platform];
const arch = platform?.arch[process.arch];
if (!platform || !arch) {
  throw new Error(`Unsupported platform for tree-sitter CLI: ${process.platform}/${process.arch}`);
}

const asset = `tree-sitter-${platform.name}-${arch}.gz`;
const url = `https://github.com/tree-sitter/tree-sitter/releases/download/v${version}/${asset}`;

console.log(`Downloading ${url}`);
const response = await fetch(url, { redirect: 'follow' });
if (!response.ok) {
  throw new Error(`Failed to download tree-sitter CLI: ${response.status} ${response.statusText}`);
}

const compressed = new Uint8Array(await response.arrayBuffer());
const executable = gunzipSync(compressed);
await Bun.write(treeSitterBinary, executable);
if (process.platform !== 'win32') {
  chmodSync(treeSitterBinary, 0o755);
}

const check = Bun.spawnSync([treeSitterBinary, '--version'], {
  stdout: 'pipe',
  stderr: 'pipe',
});
if (check.exitCode !== 0) {
  throw new Error(check.stderr.toString() || `tree-sitter CLI failed to run: ${treeSitterBinary}`);
}

console.log(check.stdout.toString().trim());
