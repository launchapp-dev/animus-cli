import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    port: 5174,
    proxy: {
      "/graphql/ws": {
        target: "http://localhost:3847",
        ws: true,
      },
      "/graphql": {
        target: "http://localhost:3847",
      },
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: [],
    globals: true,
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
  },
  build: {
    outDir: "../embedded",
    emptyOutDir: true,
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
