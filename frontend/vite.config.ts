import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: { proxy: { "/api": "http://localhost:8080" } },
  build: {
    outDir: "dist",
    // No production sourcemaps: they shipped ~1.1 MB per build and the
    // backend serves the bundle publicly.
    sourcemap: false,
    rollupOptions: {
      output: {
        // Long-lived vendor chunks: React/zod change far less often than
        // app code, so deploys only invalidate the small index chunk.
        manualChunks(id: string) {
          if (!id.includes("node_modules")) return undefined;
          if (/[\\/]react-dom|[\\/]react[\\/]|[\\/]scheduler[\\/]/.test(id)) {
            return "vendor-react";
          }
          if (id.includes("zod")) return "vendor-zod";
          return "vendor";
        },
      },
    },
  },
  test: {
    setupFiles: ["tests/setup.ts"],
  },
});
