import { readFileSync } from 'node:fs';
import type { ConfigEnv, Plugin, UserConfig } from 'vite';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { bashFor, type BashLookup } from '../../scripts/build-wasm';
import config, { PLATFORMS, resolvePlatform } from '../../vite.config';

const json = (path: string) => JSON.parse(readFileSync(new URL(path, import.meta.url), 'utf8'));
const scripts: Record<string, string> = json('../../package.json').scripts;

describe('npm scripts', () => {
  it('run in every shell: no VAR=value prefixes and no direct bash', () => {
    for (const [name, command] of Object.entries(scripts)) {
      for (const step of command.split(/&&|\|\||;/)) {
        expect(step.trim(), name).not.toMatch(/^[A-Za-z_][A-Za-z0-9_]*=/);
        expect(step.trim(), name).not.toMatch(/^(bash|sh)\s/);
      }
    }
    expect(scripts['build:wasm']).toBe('node scripts/build-wasm.ts');
  });

  it('select the platform with the Vite mode', () => {
    const modes: Record<string, string> = {
      'dev:mock': 'mock',
      'dev:desktop': 'desktop',
      'build:web': 'web',
      'build:desktop': 'desktop',
      'build:mock': 'mock',
    };
    for (const [name, mode] of Object.entries(modes)) {
      expect(scripts[name], name).toMatch(new RegExp(`\\bvite (build )?--mode ${mode}\\b`));
    }
  });

  it('include the scripts the desktop build runs', () => {
    const tauri = json('../../src-tauri/tauri.conf.json');
    for (const command of [tauri.build.beforeDevCommand, tauri.build.beforeBuildCommand]) {
      const name = /^npm run ([\w:-]+)$/.exec(command)?.[1];
      expect(name && scripts[name], command).toBeTruthy();
    }
  });
});

describe('platform of a build', () => {
  it('follows the Vite mode', () => {
    for (const platform of PLATFORMS) expect(resolvePlatform(platform, undefined)).toBe(platform);
  });

  it('falls back to VITE_PLATFORM, then web, for other modes', () => {
    expect(resolvePlatform('development', 'mock')).toBe('mock');
    expect(resolvePlatform('production', ' desktop ')).toBe('desktop');
    expect(resolvePlatform('development', undefined)).toBe('web');
    expect(resolvePlatform('production', '')).toBe('web');
  });

  it('refuses unknown or contradicting platforms', () => {
    expect(() => resolvePlatform('production', 'android')).toThrow('unknown VITE_PLATFORM: android');
    expect(() => resolvePlatform('web', 'mock')).toThrow('does not match --mode web');
    expect(resolvePlatform('desktop', 'desktop')).toBe('desktop');
  });
});

describe('vite config', () => {
  let saved: string | undefined;
  beforeEach(() => {
    saved = process.env.VITE_PLATFORM;
    delete process.env.VITE_PLATFORM;
  });
  afterEach(() => {
    if (saved === undefined) delete process.env.VITE_PLATFORM;
    else process.env.VITE_PLATFORM = saved;
  });

  const resolve = (env: ConfigEnv) => (config as (env: ConfigEnv) => UserConfig)(env);
  const pluginNames = (c: UserConfig) => (c.plugins as Plugin[]).map((p) => p.name);

  it('defines the platform from --mode', () => {
    const desktop = resolve({ command: 'build', mode: 'desktop' });
    expect(desktop.define?.__PLATFORM__).toBe('"desktop"');
    expect(pluginNames(desktop)).toEqual(['mmx-wallet:build-scope']);

    const web = resolve({ command: 'build', mode: 'web' });
    expect(web.define?.__PLATFORM__).toBe('"web"');
    expect(pluginNames(web)).toEqual(['mmx-wallet:build-scope', 'mmx:fingerprint-public-assets', 'mmx-wallet:web-security']);

    expect(resolve({ command: 'serve', mode: 'mock' }).define?.__PLATFORM__).toBe('"mock"');
  });

  it('keeps VITE_PLATFORM for a dev server without a platform mode', () => {
    process.env.VITE_PLATFORM = 'mock';
    expect(resolve({ command: 'serve', mode: 'development' }).define?.__PLATFORM__).toBe('"mock"');
  });
});

describe('bash for the WebAssembly build', () => {
  const lookup = (gitExecPath: string, installed: string[], env: BashLookup['env'] = {}): BashLookup => ({
    gitExecPath: () => gitExecPath,
    env,
    exists: (path) => installed.includes(path),
  });

  it('is bash from PATH outside Windows', () => {
    expect(bashFor('linux', lookup('', []))).toBe('bash');
    expect(bashFor('darwin', lookup('', []))).toBe('bash');
  });

  it("is Git for Windows' bash next to the git on PATH", () => {
    const bash = 'D:\\Tools\\Git\\bin\\bash.exe';
    expect(bashFor('win32', lookup('D:/Tools/Git/mingw64/libexec/git-core\n', [bash]))).toBe(bash);
  });

  it('finds the default Git for Windows installs without git on PATH', () => {
    const machine = 'C:\\Program Files\\Git\\bin\\bash.exe';
    expect(bashFor('win32', lookup('', [machine], { ProgramFiles: 'C:\\Program Files' }))).toBe(machine);
    const user = 'C:\\Users\\me\\AppData\\Local\\Programs\\Git\\bin\\bash.exe';
    expect(bashFor('win32', lookup('', [user], { LOCALAPPDATA: 'C:\\Users\\me\\AppData\\Local' }))).toBe(user);
  });

  it("never falls back to WSL's bash", () => {
    const wsl = 'C:\\Windows\\System32\\bash.exe';
    expect(bashFor('win32', lookup('/usr/lib/git-core', [wsl], { SystemRoot: 'C:\\Windows' }))).toBeUndefined();
  });
});
