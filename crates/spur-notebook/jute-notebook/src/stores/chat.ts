import { create } from "zustand";

export const DEFAULT_CHAT_APP_KEY = "notebook";
const DEFAULT_CHAT_SCOPE_LABEL = "Notebook";

export type PermissionOption = {
  id: string;
  label: string;
};

export type PendingPermission = {
  id: string;
  title: string;
  options: PermissionOption[];
};

export type ChatEvent =
  | { type: "messageChunk"; text: string }
  | { type: "toolCall"; name: string; argsSummary: string }
  | { type: "toolResult"; summary: string }
  | {
      type: "permissionRequest";
      id: string;
      title: string;
      options: PermissionOption[];
    }
  | { type: "usage"; input?: number | null; output?: number | null }
  | { type: "done" }
  | { type: "error"; message: string };

export type ChatMessage =
  | {
      id: string;
      kind: "user";
      text: string;
    }
  | {
      id: string;
      kind: "assistant";
      text: string;
    }
  | {
      id: string;
      kind: "toolCall";
      text: string;
      name: string;
      argsSummary: string;
    }
  | {
      id: string;
      kind: "toolResult";
      text: string;
    }
  | {
      id: string;
      kind: "error";
      text: string;
    };

export type ChatConversation = {
  scopeLabel: string;
  messages: ChatMessage[];
  streaming: boolean;
  streamingText: string;
  pendingPermission: PendingPermission | null;
};

export type ChatState = ChatConversation & {
  activeAppKey: string;
  conversations: Record<string, ChatConversation>;
};

export type ChatActions = {
  setScope: (appKey: string, label: string) => void;
  appendUserMessageForScope: (appKey: string, text: string) => void;
  appendUserMessage: (text: string) => void;
  applyEventForScope: (appKey: string, event: ChatEvent) => void;
  applyEvent: (event: ChatEvent) => void;
  applyEventToApp: (appKey: string, event: ChatEvent) => void;
  clearPendingPermissionForScope: (appKey: string, requestId: string) => void;
  clearPendingPermission: (requestId: string) => void;
  reset: () => void;
};

export type ChatStore = ChatState & ChatActions;

let nextMessageOrdinal = 1;

function nextMessageId(): string {
  return `chat-message-${nextMessageOrdinal++}`;
}

function createConversation(
  scopeLabel = DEFAULT_CHAT_SCOPE_LABEL,
): ChatConversation {
  return {
    scopeLabel,
    messages: [],
    streaming: false,
    streamingText: "",
    pendingPermission: null,
  };
}

function createInitialState(): ChatState {
  const defaultConversation = createConversation();
  return {
    ...defaultConversation,
    activeAppKey: DEFAULT_CHAT_APP_KEY,
    conversations: {
      [DEFAULT_CHAT_APP_KEY]: defaultConversation,
    },
  };
}

function projectActiveConversation(
  state: Pick<ChatState, "activeAppKey" | "conversations">,
): ChatState {
  const activeConversation =
    state.conversations[state.activeAppKey] ?? createConversation();
  return {
    ...activeConversation,
    activeAppKey: state.activeAppKey,
    conversations: state.conversations,
  };
}

function assistantMessage(text: string): ChatMessage {
  return {
    id: nextMessageId(),
    kind: "assistant",
    text,
  };
}

function userMessage(text: string): ChatMessage {
  return {
    id: nextMessageId(),
    kind: "user",
    text,
  };
}

function reduceConversation(
  conversation: ChatConversation,
  event: ChatEvent,
): ChatConversation {
  switch (event.type) {
    case "messageChunk":
      return {
        ...conversation,
        streaming: true,
        streamingText: conversation.streamingText + event.text,
      };
    case "done":
      if (!conversation.streamingText) {
        return { ...conversation, streaming: false };
      }
      return {
        ...conversation,
        messages: [
          ...conversation.messages,
          assistantMessage(conversation.streamingText),
        ],
        streaming: false,
        streamingText: "",
      };
    case "permissionRequest":
      return {
        ...conversation,
        pendingPermission: {
          id: event.id,
          title: event.title,
          options: event.options.map((option) => ({
            id: option.id,
            label: option.label,
          })),
        },
      };
    case "toolCall":
      return {
        ...conversation,
        messages: [
          ...conversation.messages,
          {
            id: nextMessageId(),
            kind: "toolCall",
            name: event.name,
            argsSummary: event.argsSummary,
            text: event.argsSummary
              ? `${event.name}: ${event.argsSummary}`
              : event.name,
          },
        ],
      };
    case "toolResult":
      return {
        ...conversation,
        messages: [
          ...conversation.messages,
          {
            id: nextMessageId(),
            kind: "toolResult",
            text: event.summary,
          },
        ],
      };
    case "error":
      return {
        ...conversation,
        messages: [
          ...conversation.messages,
          {
            id: nextMessageId(),
            kind: "error",
            text: event.message,
          },
        ],
        streaming: false,
      };
    case "usage":
      return conversation;
  }
}

function applyEventToAppState(
  state: ChatState,
  appKey: string,
  event: ChatEvent,
): ChatState {
  const conversation =
    state.conversations[appKey] ??
    createConversation(
      appKey === state.activeAppKey ? state.scopeLabel : undefined,
    );
  const nextConversation = reduceConversation(conversation, event);
  return projectActiveConversation({
    activeAppKey: state.activeAppKey,
    conversations: {
      ...state.conversations,
      [appKey]: nextConversation,
    },
  });
}

export const useChat = create<ChatStore>()((set, get) => ({
  ...createInitialState(),

  setScope: (appKey, label) =>
    set((state) => {
      const existing = state.conversations[appKey];
      const nextConversation = existing
        ? { ...existing, scopeLabel: label }
        : createConversation(label);
      return projectActiveConversation({
        activeAppKey: appKey,
        conversations: {
          ...state.conversations,
          [appKey]: nextConversation,
        },
      });
    }),

  applyEventForScope: (appKey, event) =>
    set((state) => {
      return applyEventToAppState(state, appKey, event);
    }),

  appendUserMessageForScope: (appKey, text) =>
    set((state) => {
      const conversation =
        state.conversations[appKey] ??
        createConversation(
          appKey === state.activeAppKey ? state.scopeLabel : undefined,
        );
      return projectActiveConversation({
        activeAppKey: state.activeAppKey,
        conversations: {
          ...state.conversations,
          [appKey]: {
            ...conversation,
            messages: [...conversation.messages, userMessage(text)],
          },
        },
      });
    }),

  appendUserMessage: (text) => {
    const state = get();
    state.appendUserMessageForScope(state.activeAppKey, text);
  },

  applyEvent: (event) => {
    const state = get();
    state.applyEventForScope(state.activeAppKey, event);
  },

  applyEventToApp: (appKey, event) => {
    const state = get();
    state.applyEventForScope(appKey, event);
  },

  clearPendingPermissionForScope: (appKey, requestId) =>
    set((state) => {
      const conversation = state.conversations[appKey];
      if (!conversation?.pendingPermission) {
        return projectActiveConversation(state);
      }
      const conversations =
        conversation.pendingPermission.id === requestId
          ? {
              ...state.conversations,
              [appKey]: { ...conversation, pendingPermission: null },
            }
          : state.conversations;
      return projectActiveConversation({
        activeAppKey: state.activeAppKey,
        conversations,
      });
    }),

  clearPendingPermission: (requestId) =>
    set((state) => {
      const conversations = Object.fromEntries(
        Object.entries(state.conversations).map(([appKey, conversation]) => [
          appKey,
          conversation.pendingPermission?.id === requestId
            ? { ...conversation, pendingPermission: null }
            : conversation,
        ]),
      );
      return projectActiveConversation({
        activeAppKey: state.activeAppKey,
        conversations,
      });
    }),

  reset: () => {
    nextMessageOrdinal = 1;
    set(createInitialState());
  },
}));
