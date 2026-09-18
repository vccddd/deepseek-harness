import { fork, type ChildProcess } from 'node:child_process'
import { PassThrough } from 'node:stream'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import {
  createStdioDesktopHostControl,
  parseDesktopHostCommand,
  type DesktopHostCommand,
} from '../src/control.ts'

const ipcFixture = fileURLToPath(new URL('control-ipc-fixture.ts', import.meta.url))

/** Commands the parity suite sends through both transports unchanged; every
 * member carries a requestId, which the replies echo back. */
const parityCommands = [
  { type: 'update-tasks', requestId: 11, action: 'inspect' },
  { type: 'update-tasks', requestId: 12, action: 'lock' },
  { type: 'update-tasks', requestId: 13, action: 'unlock' },
] as const satisfies readonly DesktopHostCommand[]

/** A Host-to-shell event frame, which neither transport may deliver as a command. */
const nonCommandFrame = { type: 'ready', url: 'http://127.0.0.1:19387/', injections: [] } as const

function frameType(frame: unknown): unknown {
  if (typeof frame !== 'object' || frame === null) return undefined
  return (frame as { type?: unknown }).type
}

async function waitFor(subject: string, condition: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 500; attempt += 1) {
    if (condition()) return
    await new Promise((resolve) => { setTimeout(resolve, 10) })
  }
  throw new Error(`timed out waiting for ${subject}`)
}

/**
 * Fork the IPC fixture over a real Node IPC channel.
 * @returns the child, the Host-role events it reported, and its settled exit code.
 */
function forkIpcFixture(): { child: ChildProcess; events: unknown[]; exited: Promise<number | null> } {
  const child = fork(ipcFixture, { execArgv: [], silent: true })
  const events: unknown[] = []
  // Registered at fork time: the fixture can exit before a waiter gets its first look.
  const exited = new Promise<number | null>((resolve) => { child.once('exit', (code) => { resolve(code) }) })
  child.on('message', (message: unknown) => { events.push(message) })
  return { child, events, exited }
}

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

describe('transport parity between Electron IPC and stdio', () => {
  it('delivers the same command stream over both transports', async () => {
    const input = new PassThrough()
    const output = new PassThrough()
    const stdio = createStdioDesktopHostControl(input, output)
    const delivered: DesktopHostCommand[] = []
    stdio.onCommand((command) => { delivered.push(command) })
    const frames = [...parityCommands.map(command => JSON.stringify(command)), JSON.stringify(nonCommandFrame)]
    for (const frame of frames) input.write(`${frame}\n`)

    const { child, events } = forkIpcFixture()
    try {
      await waitFor('fixture ready', () => events.some(event => frameType(event) === 'ready'))
      for (const frame of [...parityCommands, nonCommandFrame]) child.send(frame)
      await waitFor(
        'one update-tasks reply per command',
        () => events.filter(event => frameType(event) === 'update-tasks').length >= parityCommands.length,
      )
      // The reply requestId and the echoed command set both derive from what each
      // transport actually delivered, so equal shapes prove equal delivery.
      const replies = events
        .filter(event => frameType(event) === 'update-tasks')
        .map(event => (event as { requestId: number }).requestId)
      expect(delivered).toEqual([...parityCommands])
      expect(replies).toEqual(parityCommands.map(command => command.requestId))
    } finally {
      child.kill()
    }
  })

  it('carries the ready event, ignores non-command frames, and shuts down cleanly over IPC', async () => {
    const { child, events, exited } = forkIpcFixture()
    try {
      await waitFor('fixture ready', () => events.some(event => frameType(event) === 'ready'))
      const before = events.length
      child.send(nonCommandFrame)
      await new Promise((resolve) => { setTimeout(resolve, 100) })
      expect(events.length).toBe(before)

      child.send({ type: 'shutdown' } as const)
      await waitFor('shutdown-complete', () => events.some(event => frameType(event) === 'shutdown-complete'))
      await expect(exited).resolves.toBe(0)
      expect(events.find(event => frameType(event) === 'ready')).toEqual({
        type: 'ready',
        url: 'http://127.0.0.1:19387/auth',
        injections: [{ kind: 'style', content: 'body{}' }],
      })
    } finally {
      child.kill()
    }
  })
})
