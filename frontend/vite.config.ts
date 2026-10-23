import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  build: {
    rolldownOptions: {
      preserveEntrySignatures: false,
      output: {
        // Split large application/model bundles without raising the warning limit.
        codeSplitting: {
          includeDependenciesRecursively: false,
          minSize: 20_000,
          maxSize: 400_000,
          groups: [
            { name: 'inference', test: /node_modules[\\/](?:@huggingface|onnxruntime)/, entriesAware: true, priority: 20 },
            { name: 'app', test: /[\\/]frontend[\\/]src[\\/]/, entriesAware: true, priority: 5 },
          ],
        },
        strictExecutionOrder: true,
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
  },
})
