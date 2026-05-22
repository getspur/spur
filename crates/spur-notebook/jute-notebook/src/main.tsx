import "@fontsource-variable/fira-code";
import { invoke } from "@tauri-apps/api/core";
import { createRoot } from "react-dom/client";

import { registerAgentBridge } from "@/agent/bridge";

import App from "./App";

if (import.meta.env.MODE === "development") {
  // For development purposes, save the invoke() function in global scope.
  (window as any).invoke = invoke;
}

void registerAgentBridge();

const root = createRoot(document.getElementById("root")!);
root.render(<App />);
