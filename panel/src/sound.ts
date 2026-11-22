import type { ChatEvent } from './api'

/** What the chat tells with a sound: the assistant finished, or it waits
 * for the person's answer. */
export type Cue = 'finished' | 'waiting'

/** A shorter turn ends without a sound: the person is still looking. A
 * request sounds however soon it comes: the work stops until the answer. */
const SHORTEST_TURN_MS = 5000

/** Follows the events of a chat and says what each is worth, seen live and
 * never as found in a snapshot: a turn that starts to wait for an answer, or
 * one that ends well, not stopped (as damasco also does when it restarts)
 * and without an error. */
export function watchTurn(now: () => number = Date.now): (event: ChatEvent) => Cue | null {
  let turn: { since: number; waiting: boolean; stopped: boolean; failed: boolean } | null = null
  return (event) => {
    // The preview that could not be updated is only an item, not a problem.
    if (event.type === 'item' && turn && event.item.kind === 'error') turn.failed = true
    if (event.type !== 'snapshot' && event.type !== 'turn') return null

    if (event.turn) {
      turn ??= { since: now() - event.turn.elapsed_ms, waiting: false, stopped: false, failed: false }
      const asks = event.turn.waiting && !turn.waiting && !event.turn.stopping
      turn.waiting = event.turn.waiting
      turn.stopped ||= event.turn.stopping
      return event.type === 'turn' && asks ? 'waiting' : null
    }
    const ended = turn
    turn = null
    const well =
      event.type === 'turn' &&
      ended !== null &&
      !ended.stopped &&
      !ended.failed &&
      !event.problem &&
      now() - ended.since >= SHORTEST_TURN_MS
    return well ? 'finished' : null
  }
}

const FADE_SECONDS = 0.06

function note(hertz: number, t: number, attack: number, decay: number): number {
  if (t <= 0) return 0
  const level =
    t < attack ? Math.sin((Math.PI / 2) * (t / attack)) ** 2 : Math.exp(-(t - attack) / decay)
  return level * Math.sin(2 * Math.PI * hertz * t)
}

/** The sounds, made here without audio files, at a medium volume that is
 * the same to the ear. */
const SOUNDS: Record<Cue, { seconds: number; gain: number; at: (t: number) => number }> = {
  // A wooden bar, struck once.
  finished: {
    seconds: 0.55,
    gain: 0.13568,
    at: (t) => note(523.25, t, 0.005, 0.085) + 0.12 * note(523.25 * 3.93, t, 0.003, 0.018),
  },
  // Two notes that come down, as something put down in front of the person.
  waiting: {
    seconds: 0.85,
    gain: 0.11635,
    at: (t) => 0.7 * note(587.33, t, 0.014, 0.07) + note(440, t - 0.12, 0.018, 0.13),
  },
}

/** The samples of the sound of `cue`. */
export function samplesOf(cue: Cue, rate: number): Float32Array<ArrayBuffer> {
  const { seconds, gain, at } = SOUNDS[cue]
  const samples = new Float32Array(Math.ceil(seconds * rate))
  for (let i = 0; i < samples.length; i++) {
    const t = i / rate
    samples[i] = gain * at(t) * Math.min(1, (seconds - t) / FADE_SECONDS)
  }
  samples[samples.length - 1] = 0
  return samples
}

const KEY = 'damasco.chat.sound'
/** The choice, when the browser does not remember it. */
let chosen = true

/** Whether the sounds are on: they are, until turned off in this browser. */
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

/** Turns the sounds on or off; turned on, the one of the end is heard. */
export function setSound(on: boolean) {
  chosen = on
  try {
    localStorage.setItem(KEY, on ? 'on' : 'off')
  } catch {
    // Not remembered: the choice lasts until the page is closed.
  }
  for (const listener of listeners) listener()
  if (on) void play('finished')
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
async function play(cue: Cue, allowed?: () => Promise<boolean>) {
  uses++
  try {
    context ??= new AudioContext()
    // Refused, resume() waits for a gesture: too late for this sound.
    await Promise.race([context.resume(), new Promise((resolve) => setTimeout(resolve, 1000))])
    if (context.state !== 'running') return
    if (allowed && !(await allowed())) return
    const samples = samplesOf(cue, context.sampleRate)
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
    // Without sound, the chat still shows where the assistant is.
  } finally {
    uses--
    rest()
  }
}

/** One tab of the panel sounds for all: the first that can play holds a
 * lock while the others hear of the same event. One lock for each sound: an
 * end can follow an answer at once. Without locks (the panel over plain
 * HTTP) every tab sounds. */
function firstTab(cue: Cue): Promise<boolean> {
  if (!navigator.locks) return Promise.resolve(true)
  return new Promise((resolve) => {
    navigator.locks
      .request(`${KEY}.${cue}`, { ifAvailable: true }, async (lock) => {
        resolve(lock !== null)
        if (lock) await new Promise((release) => setTimeout(release, 2000))
      })
      .catch(() => resolve(true))
  })
}

/** Sounds `cue`, if the sounds are on. */
export function playCue(cue: Cue) {
  if (soundOn()) void play(cue, () => firstTab(cue))
}
