import { defineConfig } from "vitest/config";
import path from "node:path";

const remotePnpmFsAllow =
  process.env.SPUR_REMOTE_PNPM_VIRTUAL_STORE === "1" ? ["/mnt/cargo"] : [];

export default defineConfig({
  server: {
    fs: {
      allow: [__dirname, ...remotePnpmFsAllow],
    },
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
    },
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["@testing-library/jest-dom/vitest"],
  },
});
