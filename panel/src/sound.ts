import type { ChatEvent } from './api'

/** A shorter turn ends without a sound: the person is still looking. */
const SHORTEST_TURN_MS = 5000

/** Follows the events of a chat and says which one is a turn ending well,
 * seen live: not stopped (as damasco also does when it restarts), without
 * an error, and not found already over in a snapshot. */
export function watchFinish(now: () => number = Date.now): (event: ChatEvent) => boolean {
  let turn: { since: number; stopped: boolean; failed: boolean } | null = null
  return (event) => {
    // The preview that could not be updated is only an item, not a problem.
    if (event.type === 'item' && turn && event.item.kind === 'error') turn.failed = true
    if (event.type !== 'snapshot' && event.type !== 'turn') return false

    if (event.turn) {
      turn ??= { since: now() - event.turn.elapsed_ms, stopped: false, failed: false }
      turn.stopped ||= event.turn.stopping
      return false
    }
    const ended = turn
    turn = null
    return (
      event.type === 'turn' &&
      ended !== null &&
      !ended.stopped &&
      !ended.failed &&
      !event.problem &&
      now() - ended.since >= SHORTEST_TURN_MS
    )
  }
}

const SECONDS = 0.28
const FADE_SECONDS = 0.06
/** A medium volume: peaks at about -13.5 dBFS. */
const GAIN = 0.19467

function note(hertz: number, t: number, attack: number, decay: number): number {
  const level =
    t < attack ? Math.sin((Math.PI / 2) * (t / attack)) ** 2 : Math.exp(-(t - attack) / decay)
  return level * Math.sin(2 * Math.PI * hertz * t)
}

/** The sound, a short felted tap: made here, without an audio file. */
export function felt(rate: number): Float32Array<ArrayBuffer> {
  const samples = new Float32Array(Math.ceil(SECONDS * rate))
  for (let i = 0; i < samples.length; i++) {
    const t = i / rate
    const tap = note(392, t, 0.008, 0.045) + 0.25 * note(784, t, 0.006, 0.02)
    samples[i] = GAIN * tap * Math.min(1, (SECONDS - t) / FADE_SECONDS)
  }
  samples[samples.length - 1] = 0
  return samples
}

const KEY = 'damasco.chat.sound'
/** The choice, when the browser does not remember it. */
let chosen = true

/** Whether the sound is on: it is, until turned off in this browser. */
export function soundOn(): boolean {
  try {
    const saved = localStorage.getItem(KEY)
    return saved === null ? chosen : saved !== 'off'
  } catch {
    return chosen
  }
}

const listeners = new Set<() => void>()

/** Follows the choice, also when another tab changes it. */
export function onSoundChange(listener: () => void): () => void {
  listeners.add(listener)
  window.addEventListener('storage', listener)
  return () => {
    listeners.delete(listener)
    window.removeEventListener('storage', listener)
  }
}

/** Turns the sound on or off; turned on, it is heard once. */
export function setSound(on: boolean) {
  chosen = on
  try {
    localStorage.setItem(KEY, on ? 'on' : 'off')
  } catch {
    // Not remembered: the choice lasts until the page is closed.
  }
  for (const listener of listeners) listener()
  if (on) void play()
}

let context: AudioContext | null = null
/** Sounds starting or playing: without one the context is suspended, so
 * that it does not keep the audio device awake. */
let uses = 0

function rest() {
  if (uses === 0) context?.suspend().catch(() => {})
}

/** Browsers give sound only to a page the person used: the first click or
 * key prepares it for a turn that ends later, in a tab in the background. */
export function prepareSound(): () => void {
  const remove = () => {
    window.removeEventListener('click', prepare, true)
    window.removeEventListener('keydown', prepare, true)
  }
  const prepare = () => {
    if (!soundOn()) return
    context ??= new AudioContext()
    context.resume().then(() => {
      remove()
      rest()
    }, () => {})
  }
  window.addEventListener('click', prepare, true)
  window.addEventListener('keydown', prepare, true)
  return remove
}

/** Plays the sound, unless the browser refuses it or `allowed` says no. */
async function play(allowed?: () => Promise<boolean>) {
  uses++
  try {
    context ??= new AudioContext()
    // Refused, resume() waits for a gesture: too late for this sound.
    await Promise.race([context.resume(), new Promise((resolve) => setTimeout(resolve, 1000))])
    if (context.state !== 'running') return
    if (allowed && !(await allowed())) return
    const samples = felt(context.sampleRate)
    const buffer = context.createBuffer(1, samples.length, context.sampleRate)
    buffer.copyToChannel(samples, 0)
    const source = context.createBufferSource()
    source.buffer = buffer
    source.connect(context.destination)
    const ended = new Promise((resolve) => (source.onended = resolve))
    source.start()
    await ended
    // What the device has yet to play.
    await new Promise((resolve) => setTimeout(resolve, 300))
  } catch {
    // Without sound, the chat still shows that the assistant finished.
  } finally {
    uses--
    rest()
  }
}

/** One tab of the panel sounds for all: the first that can play holds a
 * lock while the others hear of the same ending. Without locks (the panel
 * over plain HTTP) every tab sounds. */
function firstTab(): Promise<boolean> {
  if (!navigator.locks) return Promise.resolve(true)
  return new Promise((resolve) => {
    navigator.locks
      .request(KEY, { ifAvailable: true }, async (lock) => {
        resolve(lock !== null)
        if (lock) await new Promise((release) => setTimeout(release, 2000))
      })
      .catch(() => resolve(true))
  })
}

/** The assistant finished. */
export function playFinished() {
  if (soundOn()) void play(firstTab)
}
