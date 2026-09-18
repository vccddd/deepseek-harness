/** Build and launch the unpackaged Tauri shell against the current workspace. */

import { execFileSync, spawn } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import { DESKTOP_HOST_PROTOCOL_VERSION } from '../../desktop/src/host-protocol.ts'
import { resolveDesktopPaths } from '../../desktop/src/paths.ts'
import { DesktopProjectManager } from '../../desktop/src/project-manager.ts'
import type { DesktopRelease } from '../../desktop/src/release.ts'
import { prepareDevelopmentProject } from '../../desktop/scripts/development-project.ts'
import { preparePrimaryRuntime } from '../../desktop/scripts/prepare-primary-runtime.ts'

const APP_ROOT = resolve(import.meta.dirname, '..')
const REPOSITORY_ROOT = resolve(APP_ROOT, '..', '..')
const ELECTRON_APP_ROOT = join(REPOSITORY_ROOT, 'apps', 'desktop')
const DEVELOPMENT_ROOT = join(ELECTRON_APP_ROOT, '.desktop-build', 'development')

interface PackageManifest {
  readonly version?: string
}

function packageVersion(path: string, subject: string): string {
  const manifest = JSON.parse(readFileSync(path, 'utf8')) as PackageManifest
  if (typeof manifest.version !== 'string') throw new Error(`desktop tauri development: ${subject} has no version`)
  return manifest.version
}

function nodeVersion(): string {
  return execFileSync('node', ['-p', 'process.versions.node'], { encoding: 'utf8' }).trim()
}

async function run(command: string, args: readonly string[], cwd: string, environment: NodeJS.ProcessEnv = process.env): Promise<void> {
  await new Promise<void>((resolvePromise, reject) => {
    const child = spawn(command, args, { cwd, env: environment, stdio: 'inherit' })
    child.once('error', reject)
    child.once('exit', (code, signal) => {
      if (code === 0) resolvePromise()
      else reject(new Error(`desktop tauri development: ${args.join(' ')} exited with ${String(code ?? signal)}`))
    })
  })
}

async function runPackageScript(script: string, cwd: string): Promise<void> {
  const packageManager = process.env.npm_execpath
  if (packageManager === undefined || packageManager === '') {
    throw new Error('desktop tauri development: invoke this launcher through pnpm run dev:desktop-tauri or start:desktop-tauri')
  }
  await run(process.execPath, [packageManager, 'run', script], cwd)
}

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      'skip-build': { type: 'boolean', default: false },
      'skip-prepare': { type: 'boolean', default: false },
    },
  })
  if (!values['skip-build'] && !values['skip-prepare']) {
    await runPackageScript('build', REPOSITORY_ROOT)
  }
  const hostEntry = join(REPOSITORY_ROOT, 'apps', 'desktop-host', 'lib', 'index.js')
  if (!existsSync(hostEntry)) throw new Error('desktop tauri development: missing built artifact apps/desktop-host/lib/index.js')
  if (!values['skip-prepare']) {
    const release: DesktopRelease = {
      schemaVersion: 1,
      version: packageVersion(join(APP_ROOT, 'package.json'), 'desktop tauri package'),
      hostProtocolVersion: DESKTOP_HOST_PROTOCOL_VERSION,
      nodeVersion: nodeVersion(),
      pnpmVersion: packageVersion(join(ELECTRON_APP_ROOT, 'node_modules', 'pnpm', 'package.json'), 'pnpm package'),
    }
    prepareDevelopmentProject({
      projectDir: join(DEVELOPMENT_ROOT, 'project'),
      cliDir: join(REPOSITORY_ROOT, 'apps', 'cli'),
      hostDir: join(REPOSITORY_ROOT, 'apps', 'desktop-host'),
      dependencyDir: join(REPOSITORY_ROOT, 'node_modules', '.pnpm', 'node_modules'),
      release,
    })
    await preparePrimaryRuntime()
    // The Electron shell initializes its profile at every startup; the Tauri
    // prototype keeps that step in the development launcher instead.
    const home = join(DEVELOPMENT_ROOT, 'home-tauri')
    await new DesktopProjectManager(resolveDesktopPaths(home), { dsh: join(DEVELOPMENT_ROOT, 'project') }).applyRelease(false)
    console.log(`desktop tauri development: DSH_HOME=${home}`)
  }
  console.log('desktop tauri development: launching tauri dev')
  const packageManager = process.env.npm_execpath
  if (packageManager === undefined || packageManager === '') {
    throw new Error('desktop tauri development: invoke this launcher through pnpm run dev:desktop-tauri or start:desktop-tauri')
  }
  await run(process.execPath, [packageManager, 'exec', 'tauri', 'dev'], APP_ROOT, {
    ...process.env,
    DSH_TAURI_REPO_ROOT: REPOSITORY_ROOT,
  })
}

await main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error)
  process.exitCode = 1
})
