import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 要求 base 使用相对路径，构建产物输出到 dist/（tauri.conf.json frontendDist 指向此处）。
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: { outDir: "dist", emptyOutDir: true },
  server: { port: 1420, strictPort: true },
  clearScreen: false,
});
