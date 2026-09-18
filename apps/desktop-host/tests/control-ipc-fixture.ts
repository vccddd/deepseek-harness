/** Electron-IPC fixture for the control transport parity tests. */

import { createDesktopHostControl } from '../src/control.ts'

const control = createDesktopHostControl({})
control.onCommand((command) => {
  void (async () => {
    if (command.type === 'shutdown') {
      await control.send({ type: 'shutdown-complete' })
      process.disconnect()
      return
    }
    await control.send({ type: 'update-tasks', requestId: command.requestId, active: false })
  })().catch((error: unknown) => { console.error(error) })
})
control.onDisconnect(() => { process.exit(0) })
await control.send({
  type: 'ready',
  url: 'http://127.0.0.1:19387/auth',
  injections: [{ kind: 'style', content: 'body{}' }],
})
