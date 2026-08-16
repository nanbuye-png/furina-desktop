import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 要求 base 使用相对路径，构建产物输出到 dist/（tauri.conf.json frontendDist 指向此处）。
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        manualChunks(id) {
          const normalizedId = id.replaceAll("\\", "/");
          if (!normalizedId.includes("/node_modules/")) return undefined;
          if (
            normalizedId.includes("/three/") ||
            normalizedId.includes("/@pixiv/three-vrm/")
          ) {
            return "avatar-vendor";
          }
          if (
            normalizedId.includes("/react/") ||
            normalizedId.includes("/react-dom/") ||
            normalizedId.includes("/scheduler/")
          ) {
            return "react-vendor";
          }
          return undefined;
        },
      },
    },
  },
  test: { environment: "node" },
  server: { port: 1420, strictPort: true },
  clearScreen: false,
});
