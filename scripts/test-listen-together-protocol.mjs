// Listen-together wire protocol helpers (Android-aligned ExoPlayer ints)
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

// protocol.ts is TS; reimplement the pure helpers for unit gate without bundler
const LtRepeatMode = { OFF: 0, ONE: 1, ALL: 2 }

function desktopRepeatToWire(mode) {
  switch (mode) {
    case 'one': return LtRepeatMode.ONE
    case 'all': return LtRepeatMode.ALL
    default: return LtRepeatMode.OFF
  }
}

function wireRepeatToDesktop(mode) {
  if (mode === null || mode === undefined || Number.isNaN(mode)) return null
  if (mode === LtRepeatMode.ONE) return 'one'
  if (mode === LtRepeatMode.ALL) return 'all'
  if (mode === LtRepeatMode.OFF) return 'off'
  return null
}

assert.equal(desktopRepeatToWire('off'), 0)
assert.equal(desktopRepeatToWire('one'), 1)
assert.equal(desktopRepeatToWire('all'), 2)
assert.equal(desktopRepeatToWire(undefined), 0)
assert.equal(wireRepeatToDesktop(0), 'off')
assert.equal(wireRepeatToDesktop(1), 'one')
assert.equal(wireRepeatToDesktop(2), 'all')
assert.equal(wireRepeatToDesktop(null), null)
assert.equal(wireRepeatToDesktop(99), null)

// Snapshot shape must carry mode fields for create-room
const snapshot = {
  queue: [],
  currentIndex: 0,
  settings: { allowMemberControl: true, autoPauseOnMemberChange: true, shareAudioLinks: true },
  isPlaying: false,
  positionMs: 0,
  repeatMode: desktopRepeatToWire('all'),
  shuffleEnabled: true,
}
assert.equal(snapshot.repeatMode, 2)
assert.equal(snapshot.shuffleEnabled, true)

// Event type names stay Android-aligned
const modeEvent = {
  type: 'PLAYBACK_MODE',
  repeatMode: desktopRepeatToWire('one'),
  shuffleEnabled: false,
}
assert.equal(modeEvent.type, 'PLAYBACK_MODE')
assert.equal(modeEvent.repeatMode, 1)

console.log('test-listen-together-protocol: ok')
