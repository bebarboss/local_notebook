import React from "react";
import ReactDOM from "react-dom/client";
// Fonts are bundled from node_modules — no Google Fonts request at runtime.
import "@fontsource/ibm-plex-sans-thai-looped/400.css";
import "@fontsource/ibm-plex-sans-thai-looped/500.css";
import "@fontsource/ibm-plex-sans-thai-looped/600.css";
import "@fontsource/ibm-plex-sans-thai-looped/700.css";
import "@fontsource/sarabun/400.css";
import "@fontsource/sarabun/500.css";
import "@fontsource/sarabun/600.css";
import "@fontsource/sarabun/700.css";
import "@fontsource/ibm-plex-sans-thai/400.css";
import "@fontsource/ibm-plex-sans-thai/500.css";
import "@fontsource/ibm-plex-sans-thai/600.css";
import "@fontsource/ibm-plex-sans-thai/700.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "./theme.css";
import App from "./App";
import { AppProvider } from "./state";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <AppProvider>
      <App />
    </AppProvider>
  </React.StrictMode>,
);
