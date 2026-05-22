/// <reference types="vite/client" />

// Typed shape for env vars the studio frontend reads at build time.
// Keeping this declaration explicit (rather than relying on `string | undefined`
// for every `import.meta.env.X`) catches typos at compile time.
interface ImportMetaEnv {
  readonly VITE_API_BASE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
