import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Port 1420 is what src-tauri/tauri.conf.json expects for `tauri dev`.
// Two pages: the app, and the meeting pop-up's window (src-tauri/src/meetings.rs).
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { rolldownOptions: { input: { main: "index.html", popup: "popup.html" } } },
});
