declare module '@ungap/structured-clone' {
  const structuredClone: typeof globalThis.structuredClone
  export default structuredClone
}
