/** Launch the Desktop profile through the Web application and report its URL to the desktop shell. */

import { delimiter, join } from 'node:path'
import { loadLayeredEnv, loadProfileDirectory } from '@deepseek-ai/dsh-app-boot'
import { runProfile } from '@deepseek-ai/dsh/profile-boot'
import type {} from '@deepseek-ai/dsh-client-connection'
import type {} from '@deepseek-ai/dsh-host-webserver'
import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import { createDesktopHostControl } from './control.ts'
import * as desktopOffice from './office.ts'

import { installDesktopUpdateTaskControl } from './update-tasks.ts'

async function main(): Promise<void> {
  const runtimeDir = process.argv[2] as string
  const projectDir = process.argv[3] as string
  const installAnchor = join(runtimeDir, 'node_modules', '@deepseek-ai', 'dsh', 'package.json')
  const profile = loadProfileDirectory('dsh', projectDir, installAnchor)
  const application = runProfile({
    environment: loadLayeredEnv('dsh'),
    profile: 'desktop',
    resolutionMode: process.argv[5] === 'runtime' ? 'runtime' : 'link',
    resolvedProfile: { profile, installAnchor },
    patchFiles: [],
    args: ['--no-open', '--port', '19387'],
    ...(process.argv[6] === undefined ? {} : {
      packageManager: {
        command: process.execPath,
        args: ['--expose-internals', process.argv[6]],
        env: {
          ELECTRON_RUN_AS_NODE: '1',
          DSH_DESKTOP_NODE_EXECUTABLE: process.execPath,
          PATH: `${process.argv[7] ?? ''}${delimiter}${process.env.PATH ?? ''}`,
        },
      },
    }),
  })
  let stopping: Promise<void> | undefined
  const control: { updateTasks?: ReturnType<typeof installDesktopUpdateTaskControl> } = {}
  const channel = createDesktopHostControl()
  const stop = (): Promise<void> => stopping ??= (async () => {
    // Startup failure is reported by main; shutdown only owns a tree that booted.
    const running = await application.catch(() => undefined)
    await running?.shutdown.shutdown(0)
    await channel.send({ type: 'shutdown-complete' })
    if (process.connected) process.disconnect()
  })()
  channel.onCommand((command) => {
    if (command.type === 'shutdown') { void stop(); return }
    void (async () => {
      try {
        if (stopping !== undefined || control.updateTasks === undefined) throw new Error('desktop update: Host is unavailable')
        const active = await control.updateTasks(command.action)
        await channel.send({ type: 'update-tasks', requestId: command.requestId, active })
      } catch (error) {
        await channel.send({ type: 'update-tasks', requestId: command.requestId, active: true,
          error: error instanceof Error ? error.message : String(error) })
      }
    })().catch((error: unknown) => { console.error(error) })
  })
  channel.onDisconnect(() => { void stop() })
  const { ctx } = await application
  control.updateTasks = installDesktopUpdateTaskControl(ctx)
  await ctx.plugin(desktopOffice, {
    source: process.argv[4] ?? join(runtimeDir, '..', 'runtime', 'primary-runtime'),
    root: join(resolveDshHome(), 'dsh-runtimes', 'dsh-primary-runtime'),
  })
  const url = ctx.connection.authenticatedUrl(`http://127.0.0.1:${String(ctx.webServer.port)}`)
  await channel.send({ type: 'ready', url, injections: ctx.webServer.collectIndexInjections() })
}

if (import.meta.main) {
  main().catch(async (error: unknown) => {
    const message = error instanceof Error ? error.message : String(error)
    await createDesktopHostControl().send({ type: 'fatal', message }).catch((sendError: unknown) => { console.error(sendError) })
    console.error(error)
    process.exitCode = 1
    if (process.connected) process.disconnect()
  })
}
