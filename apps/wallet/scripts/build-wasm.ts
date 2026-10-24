/**
 * Runs scripts/build-wasm.sh (the WebAssembly build of the wallet core) from
 * any shell, so `npm run build:wasm` works from cmd.exe and PowerShell too.
 *
 *   node scripts/build-wasm.ts [--test]
 *
 * On Windows the build runs in the bash of Git for Windows. A plain `bash` on
 * PATH there is often WSL's, which would build with the Linux toolchain inside
 * WSL instead of the Windows one.
 *
 * No imports outside node: so Node runs this file directly.
 */
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, realpathSync } from 'node:fs';
import { win32 } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export interface BashLookup {
  /** Output of `git --exec-path`, or '' without Git. */
  gitExecPath: () => string;
  env: Record<string, string | undefined>;
  exists: (path: string) => boolean;
}

/**
 * The bash that runs the build: `bash` from PATH, or on Windows the bash.exe
 * of Git for Windows (undefined if none is installed). Its bin\bash.exe puts
 * Git's Unix tools on PATH; GitHub's Windows runners use it for `shell: bash`.
 */
export function bashFor(platform: string, lookup: BashLookup): string | undefined {
  if (platform !== 'win32') return 'bash';
  const roots: string[] = [];
  // <git>\mingw64\libexec\git-core (clangarm64 or mingw32 on other machines).
  const execPath = lookup.gitExecPath().trim();
  if (/^[a-z]:[\\/]/i.test(execPath)) roots.push(win32.resolve(execPath, '..', '..', '..'));
  const { ProgramFiles, ProgramW6432, LOCALAPPDATA } = lookup.env;
  for (const base of [ProgramW6432, ProgramFiles, LOCALAPPDATA && win32.join(LOCALAPPDATA, 'Programs')]) {
    if (base) roots.push(win32.join(base, 'Git'));
  }
  return roots.map((root) => win32.join(root, 'bin', 'bash.exe')).find((bash) => lookup.exists(bash));
}

function gitExecPath(): string {
  try {
    return execFileSync('git', ['--exec-path'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
  } catch {
    return '';
  }
}

function main(): void {
  const bash = bashFor(process.platform, { gitExecPath, env: process.env, exists: existsSync });
  if (!bash) {
    console.error('Git for Windows is needed to build the wallet core: install it, or run scripts/build-wasm.sh from Git Bash.');
    process.exit(1);
  }
  const script = fileURLToPath(new URL('./build-wasm.sh', import.meta.url));
  const args = [process.platform === 'win32' ? script.replace(/\\/g, '/') : script, ...process.argv.slice(2)];
  const result = spawnSync(bash, args, { stdio: 'inherit' });
  if (result.error) {
    console.error(`Could not start ${bash}: ${result.error.message}`);
    process.exit(1);
  }
  if (result.signal) process.kill(process.pid, result.signal);
  process.exit(result.status ?? 1);
}

function invokedDirectly(): boolean {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    return pathToFileURL(realpathSync(entry)).href === import.meta.url;
  } catch {
    return false;
  }
}

if (invokedDirectly()) main();
