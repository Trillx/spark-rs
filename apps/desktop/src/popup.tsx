import "@fontsource-variable/jetbrains-mono";
import "@fontsource-variable/inter";
import { LucideProvider } from "lucide-react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import MeetingPopup from "./components/MeetingPopup";
import { applyTerminal, loadTerminal } from "./lib/terminal";
import { applyTheme, loadTheme } from "./lib/theme";
import "./styles.css";

// The meeting pop-up's window (popup.html). The theme is the main window's,
// which can change it while the pop-up is open.
applyTheme(loadTheme());
applyTerminal(loadTerminal());
window.addEventListener("storage", () => {
  applyTheme(loadTheme());
  applyTerminal(loadTerminal());
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <LucideProvider size={16} strokeWidth={1.75}>
      <MeetingPopup />
    </LucideProvider>
  </StrictMode>,
);
