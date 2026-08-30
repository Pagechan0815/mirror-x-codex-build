import { createRoot } from "react-dom/client";

/* ── Bundled fonts (offline, no Google Fonts request) ──
     Fontsource packages ship woff2 files that Vite bundles into dist/.
     CSS @font-face declarations are injected at build time.              */
import "@fontsource/inter";
import "@fontsource/jetbrains-mono";

const app = document.getElementById("app");

async function bootstrap() {
  if (!(app instanceof HTMLElement)) return;

  const advanced = new URLSearchParams(window.location.search).get("view") === "advanced";
  if (advanced) {
    const { App } = await import("./App");
    createRoot(app).render(<App />);
    return;
  }

  const { SimpleApp } = await import("./SimpleApp");
  createRoot(app).render(<SimpleApp />);
}

void bootstrap();
