import { Channel, invoke } from "@tauri-apps/api/core";
import clsx from "clsx";
import { BotIcon, SendIcon } from "lucide-react";
import { useEffect, useState } from "react";
import type { ChangeEvent, FormEvent } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import {
  type ChatEvent,
  type ChatMessage,
  DEFAULT_CHAT_APP_KEY,
  useChat,
} from "@/stores/chat";
import { useNotebook } from "@/stores/notebook";

const EMPTY_MESSAGES: ChatMessage[] = [];

type SessionInfo = {
  id: string;
  cwd?: string | null;
};

type AgentInfo = {
  name: string;
  label: string;
  transport?: string;
  selected: boolean;
};

function messageClassName(kind: ChatMessage["kind"]) {
  return clsx(
    "rounded border px-3 py-2 text-sm",
    kind === "assistant" && "border-gray-200 bg-white text-gray-800",
    kind === "toolCall" && "border-blue-200 bg-blue-50 text-blue-800",
    kind === "toolResult" &&
      "border-emerald-200 bg-emerald-50 text-emerald-800",
    kind === "error" && "border-red-200 bg-red-50 text-red-700",
  );
}

function scopeLabelForPath(path: string | undefined) {
  if (!path) return "Notebook";
  const filename = path.split("/").filter(Boolean).at(-1);
  return filename ?? "Notebook";
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function sessionsWithSelectedSession(
  sessions: SessionInfo[],
  selectedSessionId: string | undefined,
) {
  if (
    !selectedSessionId ||
    sessions.some((session) => session.id === selectedSessionId)
  ) {
    return sessions;
  }
  return [{ id: selectedSessionId }, ...sessions];
}

export default function ChatPanel() {
  const notebook = useNotebook();
  const [notebookPath, appOpenInfo] = useStore(
    notebook.store,
    useShallow((state) => [state.viewState.path, state.viewState.appOpenInfo]),
  );
  const [prompt, setPrompt] = useState("");
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState("");
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [selectedAgentName, setSelectedAgentName] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const chatScopeKey =
    appOpenInfo?.app_root ??
    (notebookPath ? `notebook:${notebookPath}` : DEFAULT_CHAT_APP_KEY);
  const chatScopeLabel =
    appOpenInfo?.app_name ?? scopeLabelForPath(notebookPath);
  const conversation = useChat((state) => state.conversations[chatScopeKey]);
  const scopeLabel = conversation?.scopeLabel ?? chatScopeLabel;
  const messages = conversation?.messages ?? EMPTY_MESSAGES;
  const streaming = conversation?.streaming ?? false;
  const streamingText = conversation?.streamingText ?? "";
  const pendingPermission = conversation?.pendingPermission ?? null;
  const setScope = useChat((state) => state.setScope);
  const applyEventForScope = useChat((state) => state.applyEventForScope);
  const clearPendingPermissionForScope = useChat(
    (state) => state.clearPendingPermissionForScope,
  );

  useEffect(() => {
    setScope(chatScopeKey, chatScopeLabel);
  }, [chatScopeKey, chatScopeLabel, setScope]);

  useEffect(() => {
    let disposed = false;

    void (async () => {
      try {
        const nextAgents = await invoke<AgentInfo[]>("chat_agents_list");
        if (disposed) return;

        setAgents(nextAgents);
        setSelectedAgentName((currentAgentName) => {
          if (
            currentAgentName &&
            nextAgents.some((agent) => agent.name === currentAgentName)
          ) {
            return currentAgentName;
          }
          return (
            nextAgents.find((agent) => agent.selected)?.name ??
            nextAgents[0]?.name ??
            ""
          );
        });

        if (nextAgents.length === 0) {
          applyEventForScope(chatScopeKey, {
            type: "error",
            message: "No chat agent configured",
          });
        }
      } catch (error) {
        if (disposed) return;
        applyEventForScope(chatScopeKey, {
          type: "error",
          message: errorMessage(error),
        });
      }
    })();

    return () => {
      disposed = true;
    };
  }, [applyEventForScope, chatScopeKey]);

  useEffect(() => {
    setSessions([]);
    setSelectedSessionId("");

    if (!notebookPath || !selectedAgentName) return;

    let disposed = false;
    const agentName = selectedAgentName;

    const reportScopedError = (error: unknown) => {
      if (disposed) return;
      applyEventForScope(chatScopeKey, {
        type: "error",
        message: errorMessage(error),
      });
    };

    const refreshSessions = async (preferredSessionId?: string) => {
      try {
        const nextSessions = await invoke<SessionInfo[]>("chat_sessions_list", {
          agentName,
          notebookPath,
        });
        if (disposed) return false;

        const listedSessions = sessionsWithSelectedSession(
          nextSessions,
          preferredSessionId,
        );
        setSessions(listedSessions);
        setSelectedSessionId((currentSessionId) => {
          if (preferredSessionId) return preferredSessionId;
          if (
            currentSessionId &&
            listedSessions.some((session) => session.id === currentSessionId)
          ) {
            return currentSessionId;
          }
          return listedSessions[0]?.id ?? "";
        });
        return true;
      } catch (error) {
        reportScopedError(error);
        return false;
      }
    };

    void (async () => {
      const initialListLoaded = await refreshSessions();
      const sessionId = await invoke<string>("chat_new_session", {
        agentName,
        notebookPath,
      });
      if (disposed) return;
      await invoke("chat_switch_session", {
        agentName,
        notebookPath,
        sessionId,
      });
      if (disposed) return;
      setSelectedSessionId(sessionId);
      if (initialListLoaded) {
        await refreshSessions(sessionId);
      } else {
        setSessions([{ id: sessionId }]);
      }
    })().catch((error) => {
      reportScopedError(error);
    });

    return () => {
      disposed = true;
    };
  }, [applyEventForScope, chatScopeKey, notebookPath, selectedAgentName]);

  const switchAgent = (event: ChangeEvent<HTMLSelectElement>) => {
    setSelectedAgentName(event.currentTarget.value);
  };

  const switchSession = async (event: ChangeEvent<HTMLSelectElement>) => {
    const sessionId = event.currentTarget.value;
    const sessionNotebookPath = notebookPath;
    const sessionScopeKey = chatScopeKey;
    const sessionAgentName = selectedAgentName;
    setSelectedSessionId(sessionId);
    if (!sessionNotebookPath || !sessionAgentName || !sessionId) return;

    try {
      await invoke("chat_switch_session", {
        agentName: sessionAgentName,
        notebookPath: sessionNotebookPath,
        sessionId,
      });
    } catch (error) {
      applyEventForScope(sessionScopeKey, {
        type: "error",
        message: errorMessage(error),
      });
    }
  };

  const sendPrompt = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const trimmedPrompt = prompt.trim();
    const turnNotebookPath = notebookPath;
    const turnScopeKey = chatScopeKey;
    const turnAgentName = selectedAgentName;
    if (!trimmedPrompt || !turnNotebookPath || !turnAgentName || submitting)
      return;

    const onEvent = new Channel<ChatEvent>();
    onEvent.onmessage = (message) => {
      useChat.getState().applyEventForScope(turnScopeKey, message);
    };

    setPrompt("");
    setSubmitting(true);
    try {
      await invoke("chat_turn", {
        agentName: turnAgentName,
        notebookPath: turnNotebookPath,
        prompt: trimmedPrompt,
        onEvent,
      });
    } catch (error) {
      applyEventForScope(turnScopeKey, {
        type: "error",
        message: errorMessage(error),
      });
    } finally {
      setSubmitting(false);
    }
  };

  const respondToPermission = async (requestId: string, optionId: string) => {
    const permissionScopeKey = chatScopeKey;
    const permissionAgentName = selectedAgentName;
    try {
      await invoke("chat_permission_respond", {
        agentName: permissionAgentName,
        requestId,
        optionId,
      });
      clearPendingPermissionForScope(permissionScopeKey, requestId);
    } catch (error) {
      applyEventForScope(permissionScopeKey, {
        type: "error",
        message: errorMessage(error),
      });
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col gap-3 px-3 pb-16 pt-3 text-gray-700">
      <header className="space-y-2 rounded border border-gray-200 bg-white px-3 py-2">
        <div className="flex items-center gap-2">
          <BotIcon
            className="shrink-0 text-gray-500"
            size={16}
            strokeWidth={1.5}
          />
          <div className="min-w-0">
            <div className="text-[11px] uppercase tracking-wide text-gray-400">
              Active scope
            </div>
            <div className="truncate text-sm font-medium text-gray-950">
              {scopeLabel}
            </div>
          </div>
        </div>
        <div className="grid grid-cols-1 gap-2">
          {agents.length > 0 && (
            <label className="min-w-0">
              <span className="mb-1 block text-[11px] uppercase tracking-wide text-gray-400">
                Agent
              </span>
              <select
                aria-label="Agent"
                className="h-8 w-full rounded border border-gray-300 bg-white px-2 text-xs text-gray-700 outline-none transition-colors focus:border-gray-900 disabled:cursor-not-allowed disabled:bg-gray-100"
                disabled={!notebookPath}
                onChange={switchAgent}
                value={selectedAgentName}
              >
                {agents.map((agent) => (
                  <option key={agent.name} value={agent.name}>
                    {agent.label}
                  </option>
                ))}
              </select>
            </label>
          )}
          {sessions.length > 0 && (
            <label className="min-w-0">
              <span className="mb-1 block text-[11px] uppercase tracking-wide text-gray-400">
                Session
              </span>
              <select
                aria-label="Agent session"
                className="h-8 w-full rounded border border-gray-300 bg-white px-2 text-xs text-gray-700 outline-none transition-colors focus:border-gray-900 disabled:cursor-not-allowed disabled:bg-gray-100"
                disabled={!notebookPath || !selectedAgentName}
                onChange={switchSession}
                value={selectedSessionId}
              >
                {sessions.map((session) => (
                  <option key={session.id} value={session.id}>
                    {session.id}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>
      </header>

      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto pr-1">
        {messages.length === 0 && !streamingText ? (
          <p className="px-1 py-2 text-sm text-gray-400">
            Ask the agent about this notebook.
          </p>
        ) : (
          messages.map((message) => (
            <article
              className={messageClassName(message.kind)}
              key={message.id}
            >
              {message.kind === "toolCall" && (
                <div className="mb-1 text-xs font-medium uppercase tracking-wide">
                  {message.name}
                </div>
              )}
              <div className="whitespace-pre-wrap break-words">
                {message.text}
              </div>
            </article>
          ))
        )}

        {streamingText && (
          <article className="rounded border border-gray-200 bg-white px-3 py-2 text-sm text-gray-800">
            <div className="whitespace-pre-wrap break-words">
              {streamingText}
            </div>
          </article>
        )}

        {pendingPermission && (
          <section className="rounded border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-900">
            <div className="font-medium">{pendingPermission.title}</div>
            <div className="mt-2 flex flex-wrap gap-2">
              {pendingPermission.options.map((option) => (
                <button
                  className="rounded border border-amber-300 bg-white px-2 py-1 text-xs font-medium text-amber-900 transition-colors hover:border-amber-600 disabled:cursor-not-allowed disabled:opacity-50"
                  key={option.id}
                  onClick={() =>
                    void respondToPermission(pendingPermission.id, option.id)
                  }
                  type="button"
                >
                  {option.label}
                </button>
              ))}
            </div>
          </section>
        )}
      </div>

      <form className="flex items-end gap-2" onSubmit={sendPrompt}>
        <label className="min-w-0 flex-1">
          <span className="sr-only">Message</span>
          <textarea
            aria-label="Message"
            className="max-h-32 min-h-20 w-full resize-none rounded border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 outline-none transition-colors placeholder:text-gray-400 focus:border-gray-900 disabled:cursor-not-allowed disabled:bg-gray-100"
            disabled={!notebookPath || !selectedAgentName || submitting}
            onChange={(event) => setPrompt(event.currentTarget.value)}
            placeholder={
              !notebookPath
                ? "Save the notebook to chat"
                : selectedAgentName
                  ? "Message the agent"
                  : "Select an agent"
            }
            value={prompt}
          />
        </label>
        <button
          aria-label="Send message"
          className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded border border-gray-300 bg-white text-gray-600 transition-colors hover:border-gray-900 hover:text-gray-950 disabled:cursor-not-allowed disabled:border-gray-200 disabled:text-gray-300"
          disabled={
            !notebookPath ||
            !selectedAgentName ||
            !prompt.trim() ||
            submitting ||
            streaming
          }
          title="Send message"
          type="submit"
        >
          <SendIcon size={16} strokeWidth={1.5} />
        </button>
      </form>
    </div>
  );
}
