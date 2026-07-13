import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const tauriCli = require.resolve('@tauri-apps/cli/tauri.js')
const environment = {
  ...process.env,
  NERI_BUILD_EPOCH: process.env.NERI_BUILD_EPOCH || Math.floor(Date.now() / 1000).toString(),
}

const child = spawn(process.execPath, [tauriCli, ...process.argv.slice(2)], {
  env: environment,
  stdio: 'inherit',
})

child.on('error', (error) => {
  console.error(`Failed to start Tauri CLI: ${error.message}`)
  process.exitCode = 1
})

child.on('exit', (code) => {
  process.exitCode = code ?? 1
})
