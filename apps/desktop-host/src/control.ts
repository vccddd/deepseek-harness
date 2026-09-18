/** Control transport between the Desktop Host and its launching shell. */

import { createWriteStream } from 'node:fs'
import { createInterface } from 'node:readline'
import type { Readable, Writable } from 'node:stream'
import { once } from 'node:events'

/** Command the launching shell sends to the Host. */
export type DesktopHostCommand =
  | { readonly type: 'shutdown' }
  | { readonly type: 'update-tasks'; readonly requestId: number; readonly action: 'inspect' | 'lock' | 'unlock' }

/** Event the Host reports to the launching shell. */
export type DesktopHostEvent =
  | { readonly type: 'shutdown-complete' }
  | { readonly type: 'ready'; readonly url: string; readonly injections?: readonly unknown[] }
  | { readonly type: 'fatal'; readonly message: string }
  | { readonly type: 'update-tasks'; readonly requestId: number; readonly active: boolean; readonly error?: string }

/** One shell control channel: commands in, events and disconnect signals out. */
export interface DesktopHostControl {
  /** Whether a live shell peer remains on the channel. */
  readonly connected: boolean
  /** Deliver one event; resolves after the peer accepted the write. */
  send(event: DesktopHostEvent): Promise<void>
  /** Register the sole command handler for the channel. */
  onCommand(handler: (command: DesktopHostCommand) => void): void
  /** Register the sole disconnect handler fired when the shell peer is gone. */
  onDisconnect(handler: () => void): void
}

/**
 * Validate one decoded control message as a shell command.
 * @param message - Decoded channel input of unknown shape.
 * @returns the command, or undefined for any non-command frame.
 */
export function parseDesktopHostCommand(message: unknown): DesktopHostCommand | undefined {
  if (typeof message !== 'object' || message === null || !('type' in message)) return undefined
  const candidate = message as Record<string, unknown>
  if (candidate.type === 'shutdown') return { type: 'shutdown' }
  if (candidate.type !== 'update-tasks'
    || !('requestId' in candidate) || !Number.isSafeInteger(candidate.requestId)
    || !('action' in candidate) || !['inspect', 'lock', 'unlock'].includes(String(candidate.action))) return undefined
  return {
    type: 'update-tasks',
    requestId: candidate.requestId as number,
    action: candidate.action as 'inspect' | 'lock' | 'unlock',
  }
}

/**
 * Serve commands from `input` and write events to `output` as JSON lines.
 * @param input - Shell-owned command stream (Host stdin).
 * @param output - Shell-owned event stream (the same duplex descriptor by default).
 * @returns the stdio control channel.
 */
export function createStdioDesktopHostControl(input: Readable, output: Writable): DesktopHostControl {
  let closed = false
  const disconnectHandlers: (() => void)[] = []
  // After the shell exits its receive pipe disappears; the EPIPE only reports that
  // disconnect, which the close handler below already owns.
  output.on('error', (error: NodeJS.ErrnoException) => {
    if (error.code !== 'EPIPE') console.error(error)
  })
  const close = (): void => {
    if (closed) return
    closed = true
    for (const handler of disconnectHandlers) handler()
  }
  return {
    get connected(): boolean { return !closed },
    async send(event: DesktopHostEvent): Promise<void> {
      if (closed) return
      if (!output.write(`${JSON.stringify(event)}\n`)) await once(output, 'drain').catch(() => undefined)
    },
    onCommand(handler: (command: DesktopHostCommand) => void): void {
      createInterface({ input }).on('line', (line: string) => {
        if (line.trim() === '') return
        let decoded: unknown
        try { decoded = JSON.parse(line) } catch { return }
        const command = parseDesktopHostCommand(decoded)
        if (command !== undefined) handler(command)
      })
    },
    onDisconnect(handler: () => void): void {
      disconnectHandlers.push(handler)
      input.once('close', close)
      input.once('end', close)
    },
  }
}

/**
 * Select the Host control channel for this process: Electron IPC when a Node
 * IPC channel exists, otherwise JSON lines over a shell-provided duplex
 * descriptor pair (commands arrive on stdin, events return on the descriptor
 * named by the environment, which the shell wires to the same socket).
 * @param environment - Environment selecting the stdio transport and its descriptor.
 * @returns the active control channel for the launching shell.
 */
export function createDesktopHostControl(environment: NodeJS.ProcessEnv = process.env): DesktopHostControl {
  if (environment.DSH_DESKTOP_HOST_CONTROL !== 'stdio') {
    return {
      get connected(): boolean { return process.connected },
      send: event => new Promise((resolve, reject) => {
        if (!process.connected || process.send === undefined) { resolve(); return }
        process.send(event, (error) => { if (error === null) resolve(); else reject(error) })
      }),
      onCommand(handler: (command: DesktopHostCommand) => void): void {
        process.on('message', (message: unknown) => {
          const command = parseDesktopHostCommand(message)
          if (command !== undefined) handler(command)
        })
      },
      onDisconnect(handler: () => void): void { process.once('disconnect', handler) },
    }
  }
  const descriptor = Number(environment.DSH_DESKTOP_HOST_CONTROL_FD ?? '0')
  if (!Number.isSafeInteger(descriptor) || descriptor < 0) {
    throw new Error('desktop host: DSH_DESKTOP_HOST_CONTROL_FD must be a non-negative descriptor number')
  }
  // The path argument is inert once `fd` is set; an empty string satisfies
  // the file-name type without naming a file the stream would open.
  return createStdioDesktopHostControl(process.stdin, createWriteStream('', { fd: descriptor, encoding: 'utf8' }))
}
