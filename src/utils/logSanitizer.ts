export function summarizeLogError(error: unknown): string {
  const message = error instanceof Error
    ? error.stack || error.message
    : String(error)
  return message.replace(/https?:\/\/[^\s)\]}]+/g, '[url]')
}
