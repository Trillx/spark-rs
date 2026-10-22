import "@fontsource-variable/inter";
import { LucideProvider } from "lucide-react";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import TooltipLayer from "./components/TooltipLayer";
import { applyTheme, loadTheme } from "./lib/theme";
import "./styles.css";

applyTheme(loadTheme());

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {/* Spark's line icons are lighter than lucide's default stroke of 2. */}
    <LucideProvider size={16} strokeWidth={1.75}>
      <App />
      <TooltipLayer />
    </LucideProvider>
  </StrictMode>,
);
