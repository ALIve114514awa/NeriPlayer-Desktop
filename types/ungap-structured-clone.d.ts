declare module '@ungap/structured-clone' {
  export interface StructuredCloneOptions {
    transfer?: unknown[]
    json?: boolean
    lossy?: boolean
  }

  const structuredClone: <T>(value: T, options?: StructuredCloneOptions) => T

  export default structuredClone
  export function serialize(value: unknown, options?: StructuredCloneOptions): unknown
  export function deserialize(value: unknown): unknown
}
