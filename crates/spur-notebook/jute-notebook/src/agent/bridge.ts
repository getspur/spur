// The bridge registers on every app boot because the SPUR notebook binary owns
// the real Rust AgentBridge; the standalone vendored Jute shell registers
// no-op bridge commands so this shared frontend path remains valid until that
// shell grows full agent transport.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { Notebook } from "@/stores/notebook";

import { dispatchAgentRequest } from "./handlers";
import {
  AgentHandlerError,
  type AgentBridgeError,
  type AgentBridgeRequest,
  type AgentBridgeResponse,
} from "./types";

let activeNotebook: Notebook | undefined;
let registration: Promise<void> | undefined;

export function setActiveAgentNotebook(notebook: Notebook | undefined) {
  activeNotebook = notebook;
  void invoke("notebook_active_changed", { open: Boolean(notebook) });
}

export function registerAgentBridge(): Promise<void> {
  registration ??= register();
  return registration;
}

async function register() {
  await listen<AgentBridgeRequest>("agent://request", async (event) => {
    const response = await handleAgentRequest(event.payload);
    await invoke("agent_response", { payload: response });
  });
  await invoke("bridge_ready");
}

async function handleAgentRequest(
  request: AgentBridgeRequest,
): Promise<AgentBridgeResponse> {
  try {
    return {
      requestId: request.requestId,
      result: await dispatchAgentRequest(activeNotebook, request),
    };
  } catch (error) {
    return {
      requestId: request.requestId,
      error: toBridgeError(error),
    };
  }
}

function toBridgeError(error: unknown): AgentBridgeError {
  if (error instanceof AgentHandlerError) {
    return { code: error.code, message: error.message };
  }
  return {
    code: "handler_failed",
    message: error instanceof Error ? error.message : String(error),
  };
}
