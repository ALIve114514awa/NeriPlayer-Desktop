import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import ts from 'typescript'

const sourceUrl = new URL('../src/utils/nowPlayingBackground.ts', import.meta.url)
const source = await readFile(sourceUrl, 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText
const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
const { shouldShowDynamicBackground } = await import(moduleUrl)

const palette = {
  shaderColors: [],
  lightOffset: 0,
  saturateOffset: 0,
  accentBg: [18, 18, 18],
  primaryColor: [18, 18, 18],
  dominant: [18, 18, 18],
  lightVibrant: [18, 18, 18],
  muted: [18, 18, 18],
  darkMuted: [18, 18, 18],
}

assert.equal(shouldShowDynamicBackground(false, true, palette), false)
assert.equal(shouldShowDynamicBackground(true, false, palette), false)
assert.equal(shouldShowDynamicBackground(true, true, null), false)
assert.equal(shouldShowDynamicBackground(true, true, palette), true)

console.log('now playing background tests passed')
