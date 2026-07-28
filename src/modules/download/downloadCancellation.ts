export interface DownloadCancellationTask {
  trackId: string
  status: string
}

export interface ResolvingCancellation {
  trackId: string
  token: string
}

export function markResolvingTasksCancelled(
  tasks: Iterable<DownloadCancellationTask>,
  cancelled: Set<string>,
  tokenFor: (trackId: string) => string | undefined,
): ResolvingCancellation[] {
  const resolving: ResolvingCancellation[] = []
  for (const task of tasks) {
    if (task.status !== 'resolving') continue
    const token = tokenFor(task.trackId)
    if (!token) continue
    cancelled.add(token)
    resolving.push({ trackId: task.trackId, token })
  }
  return resolving
}

export function consumeResolvingCancellation(cancelled: Set<string>, token: string): boolean {
  if (!cancelled.has(token)) return false
  cancelled.delete(token)
  return true
}
