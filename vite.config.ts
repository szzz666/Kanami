import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 在移动端会把前端资产打包进去，所以必须用相对路径
export default defineConfig({
  plugins: [react()],
  base: "./",
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "0.0.0.0",
    watch: { ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**"] },
  },
  build: {
    target: ["safari15", "chrome100"],
    minify: "esbuild",
    sourcemap: false,
  },
});
