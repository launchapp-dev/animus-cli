import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    setupFiles: [],
    globals: true,
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
  },
  build: {
    outDir: "../embedded",
    emptyOutDir: false,
    cssCodeSplit: true,
    chunkSizeWarningLimit: 240,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("node_modules/react-router")) {
            return "routing-vendor";
          }
          if (id.includes("node_modules/react") || id.includes("node_modules/scheduler")) {
            return "react-vendor";
          }
          return undefined;
        },
      },
    },
  },
});
