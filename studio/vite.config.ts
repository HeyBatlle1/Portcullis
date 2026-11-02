import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Built into the Rust crate's directory, which the server serves at /audit.
// The build output is committed so the host needs no Node toolchain.
export default defineConfig({
  base: '/audit/',
  plugins: [react()],
  build: {
    outDir: '../crates/mcp/studio-dist',
    emptyOutDir: true,
    chunkSizeWarningLimit: 8000,
  },
  server: {
    proxy: {
      '/console': 'http://localhost:8787',
      '/audit/config': 'http://localhost:8787',
      '/audit/ai': 'http://localhost:8787',
    },
  },
});
