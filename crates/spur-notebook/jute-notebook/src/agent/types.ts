import type { Output } from "@/bindings";
import type { CellDagMetadata } from "@/bindings/CellDagMetadata";
import type { CodeType } from "@/bindings/CodeType";
import type { JuteDeckCellMetadata } from "@/bindings/JuteDeckCellMetadata";

type AgentBridgeRequestBase = {
  requestId: string;
};

export type AgentBridgeRequest =
  | (AgentBridgeRequestBase & {
      method: "notebook.snapshot";
      params?: unknown;
    })
  | (AgentBridgeRequestBase & {
      method: "notebook.export";
      params?: unknown;
    })
  | (AgentBridgeRequestBase & {
      method: "notebook.flush_pending";
      params?: unknown;
    })
  | (AgentBridgeRequestBase & {
      method: "notebook.read_cell";
      params: unknown;
    })
  | (AgentBridgeRequestBase & {
      method: "notebook.insert_cell";
      params: unknown;
    })
  | (AgentBridgeRequestBase & {
      method: "notebook.write_cell";
      params: unknown;
    })
  | (AgentBridgeRequestBase & {
      method: "notebook.delete_cell";
      params: unknown;
    })
  | (AgentBridgeRequestBase & {
      method: "notebook.set_cell_metadata";
      params: AgentSetCellMetadata;
    });

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

export type AgentRunCell = {
  id: string;
  status: AgentCellStatus;
  exec_count: number | null;
  outputs: Output[];
  events: [];
};

export type AgentKernelInfo = {
  kernel_id: string;
  spec_name: string;
  generation: number;
  status: string;
  cpu_pct: number;
  mem_mb: number;
};

export type AgentInsertCell = {
  id: string;
  version: number;
};

export type AgentWriteCell = {
  version: number;
};

export type AgentSetCellMetadata = {
  id: string;
  patch: Partial<JuteDeckCellMetadata> & {
    spur?: {
      datasource_setup?: boolean;
      dag?: CellDagMetadata;
      code_type?: CodeType;
    };
  };
  expected_version: number;
};

export type AgentDeleteCell = {
  deleted: true;
};
