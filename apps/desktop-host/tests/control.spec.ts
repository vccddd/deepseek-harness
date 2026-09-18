import { PassThrough } from 'node:stream'
import { describe, expect, it } from 'vitest'
import { createStdioDesktopHostControl, parseDesktopHostCommand } from '../src/control.ts'

describe('parseDesktopHostCommand', () => {
  it('accepts the shutdown command', () => {
    expect(parseDesktopHostCommand({ type: 'shutdown' })).toEqual({ type: 'shutdown' })
  })

  it('accepts a well-formed update-tasks command', () => {
    expect(parseDesktopHostCommand({ type: 'update-tasks', requestId: 7, action: 'lock' }))
      .toEqual({ type: 'update-tasks', requestId: 7, action: 'lock' })
  })

  it('rejects non-command frames without throwing', () => {
    for (const frame of [
      null, 'shutdown', 4, {}, { type: 'unknown' },
      { type: 'update-tasks', requestId: 'x', action: 'inspect' },
      { type: 'update-tasks', requestId: 1, action: 'destroy' },
    ]) {
      expect(parseDesktopHostCommand(frame)).toBeUndefined()
    }
  })
})

describe('createStdioDesktopHostControl', () => {
  it('delivers parsed commands from JSON lines and reports disconnect on end', async () => {
    const input = new PassThrough()
    const output = new PassThrough()
    const control = createStdioDesktopHostControl(input, output)
    const commands: unknown[] = []
    let disconnected = 0
    control.onCommand((command) => { commands.push(command) })
    control.onDisconnect(() => { disconnected += 1 })
    expect(control.connected).toBe(true)
    input.write(`${JSON.stringify({ type: 'shutdown' })}\n`)
    input.write('not json\n')
    input.write(`${JSON.stringify({ type: 'update-tasks', requestId: 2, action: 'inspect' })}\n`)
    input.end()
    const disconnectedOnce = new Promise<void>((resolve) => {
      const check = (): void => {
        if (disconnected > 0) resolve()
        else setImmediate(check)
      }
      setImmediate(check)
    })
    await disconnectedOnce
    expect(commands).toEqual([
      { type: 'shutdown' },
      { type: 'update-tasks', requestId: 2, action: 'inspect' },
    ])
    expect(disconnected).toBe(1)
    expect(control.connected).toBe(false)
  })

  it('serializes events as JSON lines', async () => {
    const input = new PassThrough()
    const output = new PassThrough()
    const control = createStdioDesktopHostControl(input, output)
    const seen: string[] = []
    output.on('data', (chunk: Buffer) => { seen.push(...chunk.toString('utf8').split('\n').filter(line => line !== '')) })
    await control.send({ type: 'shutdown-complete' })
    await control.send({ type: 'fatal', message: 'boom' })
    expect(seen.map(line => JSON.parse(line) as unknown)).toEqual([
      { type: 'shutdown-complete' },
      { type: 'fatal', message: 'boom' },
    ])
  })
})
