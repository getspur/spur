import type { Output } from "@/bindings";

export type AgentBridgeRequest = {
  requestId: string;
  method: string;
  params: unknown;
};

export type AgentBridgeResponse =
  | {
      requestId: string;
      result: unknown;
    }
  | {
      requestId: string;
      error: AgentBridgeError;
    };

export type AgentBridgeError = {
  code: string;
  message: string;
};

export class AgentHandlerError extends Error {
  code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "AgentHandlerError";
    this.code = code;
  }
}

export type AgentCellStatus = "idle" | "running" | "success" | "error";

export type AgentSnapshotCell = {
  id: string;
  kind: "code" | "markdown";
  version: number;
  exec_count: number | null;
  status: AgentCellStatus;
  source: string;
};

export type AgentReadCell = AgentSnapshotCell & {
  outputs: Output[];
};

export type AgentKernelInfo = {
  kernel_id: string;
  spec_name: string;
  generation: number;
  status: string;
  cpu_pct: number;
  mem_mb: number;
};
