/** Vite's `?raw` import: the file's text, bundled. The core presentation entries ship this way. */
declare module "*?raw" {
  const text: string;
  export default text;
}
