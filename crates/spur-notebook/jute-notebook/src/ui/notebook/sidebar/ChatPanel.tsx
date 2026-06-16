import { Channel, invoke } from "@tauri-apps/api/core";
import clsx from "clsx";
import { BotIcon, CircleIcon, SendIcon } from "lucide-react";
import { useEffect, useState } from "react";
import type { ChangeEvent, FormEvent, KeyboardEvent } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import {
  type ChatEvent,
  type ChatMessage,
  DEFAULT_CHAT_APP_KEY,
  useChat,
} from "@/stores/chat";
import { useNotebook } from "@/stores/notebook";

import {
  type ChatLens,
  EMPTY_STATE_COPY,
  composerLensLabel,
  defaultLensFor,
  mapViewMode,
} from "./lens";
import MarkdownRenderer from "../MarkdownRenderer";

const EMPTY_MESSAGES: ChatMessage[] = [];
const CHAT_MARKDOWN_CLASS_NAME = clsx(
  "min-w-0 break-words",
  "[&>:first-child]:mt-0 [&>:last-child]:mb-0",
  "[&_a]:text-[#f54e00] [&_a]:underline [&_a]:underline-offset-2",
  "[&_blockquote]:my-2 [&_blockquote]:border-l-2 [&_blockquote]:border-[#bfc1b7] [&_blockquote]:pl-3 [&_blockquote]:text-[#65675e]",
  "[&_code]:rounded [&_code]:bg-[#eeefe9] [&_code]:px-1 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-[12px]",
  "[&_h1]:mb-2 [&_h1]:text-base [&_h1]:font-semibold",
  "[&_h2]:mb-2 [&_h2]:text-sm [&_h2]:font-semibold",
  "[&_h3]:mb-1.5 [&_h3]:text-sm [&_h3]:font-semibold",
  "[&_li]:my-1 [&_ol]:my-2 [&_ol]:list-decimal [&_ol]:pl-5 [&_p]:my-2 [&_ul]:my-2 [&_ul]:list-disc [&_ul]:pl-5",
  "[&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-[#1e1f23] [&_pre]:p-2 [&_pre]:text-[#fdfdf8]",
);

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

type SessionModesResponse = {
  modes: string[];
  current?: string | null;
};

function messageClassName(kind: ChatMessage["kind"]) {
  return clsx(
    "max-w-[92%] rounded-md border px-3 py-2 text-[13px]",
    kind === "user" && "ml-auto border-[#1e1f23] bg-[#1e1f23] text-[#fdfdf8]",
    kind === "assistant" &&
      "mr-auto border-[#bfc1b7] bg-[#fdfdf8] text-[#23251d]",
    kind === "error" && "border-red-200 bg-red-50 text-red-700",
  );
}

function scopeLabelForPath(path: string | undefined) {
  if (!path) return "Notebook";
  const filename = path.split("/").filter(Boolean).at(-1);
  return filename ?? "Notebook";
}

function scopeHintForPath(path: string | undefined) {
  return path ?? "Save notebook to chat";
}

function statusText({
  agentsLoaded,
  hasNotebookPath,
  hasSelectedAgent,
  pendingPermission,
  streaming,
}: {
  agentsLoaded: boolean;
  hasNotebookPath: boolean;
  hasSelectedAgent: boolean;
  pendingPermission: boolean;
  streaming: boolean;
}) {
  if (pendingPermission) return "Waiting for permission";
  if (streaming) return "Streaming in this session";
  if (!hasNotebookPath) return "Save notebook to chat";
  if (agentsLoaded && !hasSelectedAgent) return "No chat agent configured";
  return "Ready with scoped tools enabled";
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

function resolvedSessionModes(response: SessionModesResponse | null | undefined) {
  const modes = response?.modes ?? [];
  const current =
    response?.current && modes.includes(response.current)
      ? response.current
      : (modes[0] ?? "");
  return { modes, current };
}

export default function ChatPanel() {
  const notebook = useNotebook();
  const [notebookPath, appOpenInfo, viewMode, selectedCellId] = useStore(
    notebook.store,
    useShallow((state) => [
      state.viewState.path,
      state.viewState.appOpenInfo,
      state.viewState.viewMode,
      state.viewState.selectedCellId,
    ]),
  );
  const [prompt, setPrompt] = useState("");
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState("");
  const [sessionModes, setSessionModes] = useState<string[]>([]);
  const [selectedSessionMode, setSelectedSessionMode] = useState("");
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [agentsLoaded, setAgentsLoaded] = useState(false);
  const [selectedAgentName, setSelectedAgentName] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [lensOverride, setLensOverride] = useState<ChatLens | null>(null);

  const defaultLens = defaultLensFor(viewMode, appOpenInfo);
  const lens =
    viewMode === "cells" ? (lensOverride ?? defaultLens) : defaultLens;
  const emptyStateCopy = EMPTY_STATE_COPY[lens];
  const chatScopeKey =
    appOpenInfo?.app_root ??
    (notebookPath ? `notebook:${notebookPath}` : DEFAULT_CHAT_APP_KEY);
  const chatScopeLabel =
    appOpenInfo?.app_name ?? scopeLabelForPath(notebookPath);
  const conversation = useChat((state) => state.conversations[chatScopeKey]);
  const scopeLabel = conversation?.scopeLabel ?? chatScopeLabel;
  const scopeHint = appOpenInfo?.app_root ?? scopeHintForPath(notebookPath);
  const messages = conversation?.messages ?? EMPTY_MESSAGES;
  const streaming = conversation?.streaming ?? false;
  const streamingText = conversation?.streamingText ?? "";
  const pendingPermission = conversation?.pendingPermission ?? null;
  const currentStatusText = statusText({
    agentsLoaded,
    hasNotebookPath: Boolean(notebookPath),
    hasSelectedAgent: Boolean(selectedAgentName),
    pendingPermission: Boolean(pendingPermission),
    streaming,
  });
  const composerStatusText = notebookPath
    ? `Ready in ${scopeLabel} - ${composerLensLabel(lens)}`
    : "Save notebook to chat";
  const setScope = useChat((state) => state.setScope);
  const applyEventForScope = useChat((state) => state.applyEventForScope);
  const clearPendingPermissionForScope = useChat(
    (state) => state.clearPendingPermissionForScope,
  );
  const appendUserMessageForScope = useChat(
    (state) => state.appendUserMessageForScope,
  );

  useEffect(() => {
    setScope(chatScopeKey, chatScopeLabel);
  }, [chatScopeKey, chatScopeLabel, setScope]);

  useEffect(() => {
    setLensOverride(null);
  }, [viewMode]);

  useEffect(() => {
    let disposed = false;

    void (async () => {
      try {
        const nextAgents = await invoke<AgentInfo[]>("chat_agents_list");
        if (disposed) return;

        setAgentsLoaded(true);
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
        setAgentsLoaded(true);
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
    setSessionModes([]);
    setSelectedSessionMode("");

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

    const refreshSessionModes = async () => {
      try {
        const response = await invoke<SessionModesResponse | null>(
          "chat_session_modes_list",
          {
            agentName,
            notebookPath,
          },
        );
        if (disposed) return;
        const { modes, current } = resolvedSessionModes(response);
        setSessionModes(modes);
        setSelectedSessionMode(current);
      } catch (error) {
        if (disposed) return;
        setSessionModes([]);
        setSelectedSessionMode("");
        reportScopedError(error);
      }
    };

    void (async () => {
      const initialListLoaded = await refreshSessions();
      const sessionId = await invoke<string>("chat_new_session", {
        agentName,
        notebookPath,
      });
      if (disposed) return;
      setSelectedSessionId(sessionId);
      if (initialListLoaded) {
        await refreshSessions(sessionId);
      } else {
        setSessions([{ id: sessionId }]);
      }
      await refreshSessionModes();
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
    setSessionModes([]);
    setSelectedSessionMode("");
    if (!sessionNotebookPath || !sessionAgentName || !sessionId) return;

    try {
      await invoke("chat_switch_session", {
        agentName: sessionAgentName,
        notebookPath: sessionNotebookPath,
        sessionId,
      });
      const response = await invoke<SessionModesResponse | null>(
        "chat_session_modes_list",
        {
          agentName: sessionAgentName,
          notebookPath: sessionNotebookPath,
        },
      );
      const { modes, current } = resolvedSessionModes(response);
      setSessionModes(modes);
      setSelectedSessionMode(current);
    } catch (error) {
      applyEventForScope(sessionScopeKey, {
        type: "error",
        message: errorMessage(error),
      });
    }
  };

  const switchSessionMode = async (event: ChangeEvent<HTMLSelectElement>) => {
    const modeId = event.currentTarget.value;
    const previousModeId = selectedSessionMode;
    const modeNotebookPath = notebookPath;
    const modeScopeKey = chatScopeKey;
    const modeAgentName = selectedAgentName;
    setSelectedSessionMode(modeId);
    if (!modeNotebookPath || !modeAgentName || !modeId) return;

    try {
      await invoke("chat_set_session_mode", {
        agentName: modeAgentName,
        notebookPath: modeNotebookPath,
        modeId,
      });
    } catch (error) {
      setSelectedSessionMode(previousModeId);
      applyEventForScope(modeScopeKey, {
        type: "error",
        message: errorMessage(error),
      });
    }
  };

  const canSubmitPrompt =
    Boolean(notebookPath) &&
    Boolean(selectedAgentName) &&
    Boolean(prompt.trim()) &&
    !submitting;

  const sendPrompt = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const trimmedPrompt = prompt.trim();
    const turnNotebookPath = notebookPath;
    const turnScopeKey = chatScopeKey;
    const turnAgentName = selectedAgentName;
    const turnViewMode = viewMode;
    const turnLens = lens;
    const turnSelectedCellId = selectedCellId;
    if (
      !canSubmitPrompt ||
      !trimmedPrompt ||
      !turnNotebookPath ||
      !turnAgentName
    )
      return;

    const onEvent = new Channel<ChatEvent>();
    onEvent.onmessage = (message) => {
      useChat.getState().applyEventForScope(turnScopeKey, message);
    };

    setPrompt("");
    setSubmitting(true);
    appendUserMessageForScope(turnScopeKey, trimmedPrompt);
    try {
      await invoke("chat_turn", {
        agentName: turnAgentName,
        notebookPath: turnNotebookPath,
        prompt: trimmedPrompt,
        context: {
          notebookPath: turnNotebookPath,
          viewMode: mapViewMode(turnViewMode),
          lens: turnLens,
          ...(turnSelectedCellId
            ? { selectedCellRef: `cell://${turnSelectedCellId}` }
            : {}),
        },
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

  const submitPromptOnEnter = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (
      event.key !== "Enter" ||
      event.shiftKey ||
      event.nativeEvent.isComposing
    )
      return;

    event.preventDefault();
    event.currentTarget.form?.requestSubmit();
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
    <div className="flex h-full min-h-0 flex-col gap-2 bg-[#fdfdf8] px-3 pb-16 pt-2 text-[#23251d]">
      <header className="space-y-2 border-b border-[#d8d9d1] bg-[#fdfdf8] pb-2">
        <div className="flex items-center gap-2">
          <BotIcon
            className="shrink-0 text-[#65675e]"
            size={14}
            strokeWidth={1.5}
          />
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-semibold leading-5 text-[#23251d]">
              {scopeLabel}
            </div>
            <div className="mt-0.5 truncate text-[11px] text-[#65675e]">
              {scopeHint}
            </div>
          </div>
          <div className="flex max-w-[44%] items-center gap-1.5 rounded-full bg-[#eeefe9] px-2 py-1 text-[11px] font-medium text-[#65675e]">
            <CircleIcon
              className={clsx(
                "h-2 w-2 shrink-0 fill-current",
                pendingPermission ? "text-[#d6b36e]" : "text-[#65675e]",
              )}
              size={8}
              strokeWidth={0}
            />
            <span className="truncate">{currentStatusText}</span>
          </div>
        </div>
        <div className="grid grid-cols-2 gap-2">
          {agents.length > 0 && (
            <label className="min-w-0">
              <span className="sr-only">Agent</span>
              <select
                aria-label="Agent"
                className="h-8 w-full rounded-md border border-[#bfc1b7] bg-[#fdfdf8] px-2 text-xs text-[#23251d] outline-none transition-colors hover:border-[#f54e00] focus:border-[#1e1f23] disabled:cursor-not-allowed disabled:bg-[#eeefe9] disabled:text-[#65675e]"
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
              <span className="sr-only">Session</span>
              <select
                aria-label="Agent session"
                className="h-8 w-full rounded-md border border-[#bfc1b7] bg-[#fdfdf8] px-2 text-xs text-[#23251d] outline-none transition-colors hover:border-[#f54e00] focus:border-[#1e1f23] disabled:cursor-not-allowed disabled:bg-[#eeefe9] disabled:text-[#65675e]"
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
          {sessionModes.length > 0 && (
            <label className="min-w-0">
              <span className="sr-only">Mode</span>
              <select
                aria-label="Agent mode"
                className="h-8 w-full rounded-md border border-[#bfc1b7] bg-[#fdfdf8] px-2 text-xs text-[#23251d] outline-none transition-colors hover:border-[#f54e00] focus:border-[#1e1f23] disabled:cursor-not-allowed disabled:bg-[#eeefe9] disabled:text-[#65675e]"
                disabled={!notebookPath || !selectedAgentName}
                onChange={switchSessionMode}
                value={selectedSessionMode}
              >
                {sessionModes.map((mode) => (
                  <option key={mode} value={mode}>
                    {mode}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>
        <div className="flex items-center gap-2 text-[11px] text-[#65675e]">
          <span className="font-medium uppercase">Lens</span>
          {viewMode === "cells" ? (
            <div className="inline-flex rounded-md border border-[#d8d9d1] bg-[#fdfdf8] p-0.5">
              {(
                [
                  ["notebook_builder", "Builder"],
                  ["notebook_deep_dive", "Deep dive"],
                ] satisfies Array<[ChatLens, string]>
              ).map(([nextLens, label]) => (
                <button
                  aria-pressed={lens === nextLens}
                  className={clsx(
                    "h-6 rounded px-2 text-[11px] font-medium transition-colors",
                    lens === nextLens
                      ? "bg-[#23251d] text-[#fdfdf8]"
                      : "text-[#65675e] hover:bg-[#eeefe9] hover:text-[#23251d]",
                  )}
                  key={nextLens}
                  onClick={() => setLensOverride(nextLens)}
                  type="button"
                >
                  {label}
                </button>
              ))}
            </div>
          ) : (
            <span className="inline-flex h-6 items-center rounded-md border border-[#d8d9d1] bg-[#eeefe9] px-2 font-medium text-[#23251d]">
              {composerLensLabel(lens).replace(/ lens$/, "")}
            </span>
          )}
        </div>
      </header>

      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto pr-1">
        {messages.length === 0 && !streamingText ? (
          <section className="rounded-md border border-[#bfc1b7] bg-[#fdfdf8] px-3 py-3">
            <div className="text-sm font-semibold text-[#23251d]">
              {emptyStateCopy.heading}
            </div>
            <p className="mt-1 text-xs leading-5 text-[#65675e]">
              {emptyStateCopy.copy}
            </p>
          </section>
        ) : (
          messages.map((message) =>
            message.kind === "toolCall" || message.kind === "toolResult" ? (
              <article
                className="flex gap-2 rounded-md border border-[#bfc1b7] bg-[#eeefe9] px-2.5 py-2 text-xs text-[#65675e]"
                key={message.id}
              >
                <div className="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-[#65675e]" />
                <div className="min-w-0 flex-1">
                  <div className="font-medium text-[#23251d]">
                    {message.kind === "toolCall"
                      ? `Tool call: ${message.name}`
                      : "Tool result"}
                  </div>
                  <div className="mt-0.5 truncate font-mono text-[11px]">
                    {message.kind === "toolCall"
                      ? message.argsSummary || message.name
                      : message.text}
                  </div>
                </div>
              </article>
            ) : (
              <article
                className={messageClassName(message.kind)}
                key={message.id}
              >
                {message.kind === "assistant" ? (
                  <MarkdownRenderer
                    className={CHAT_MARKDOWN_CLASS_NAME}
                    source={message.text}
                  />
                ) : (
                  <div className="whitespace-pre-wrap break-words">
                    {message.text}
                  </div>
                )}
              </article>
            ),
          )
        )}

        {streamingText && (
          <article className="rounded-md border border-[#bfc1b7] border-l-[#f54e00] bg-[#fdfdf8] px-3 py-2 text-[13px] text-[#23251d]">
            <div className="mb-1 text-[10px] font-medium uppercase text-[#65675e]">
              Streaming
            </div>
            <MarkdownRenderer
              className={CHAT_MARKDOWN_CLASS_NAME}
              source={streamingText}
            />
          </article>
        )}

        {pendingPermission && (
          <section className="rounded-md border border-[#d6b36e] bg-[#fff6dc] px-3 py-3 text-sm text-[#23251d]">
            <div className="text-[10px] font-semibold uppercase text-[#8a6118]">
              Permission required
            </div>
            <div className="mt-1 font-semibold">{pendingPermission.title}</div>
            <div className="mt-1 text-xs text-[#65675e]">
              Choose how the agent should continue in this scope.
            </div>
            <div className="mt-3 flex flex-wrap gap-2">
              {pendingPermission.options.map((option) => (
                <button
                  className="min-h-8 rounded-md border border-[#d6b36e] bg-[#fdfdf8] px-3 py-1 text-xs font-medium text-[#23251d] transition-colors hover:border-[#f54e00] disabled:cursor-not-allowed disabled:opacity-50"
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

      <form className="space-y-1.5" onSubmit={sendPrompt}>
        <div className="truncate text-[11px] font-medium text-[#65675e]">
          {composerStatusText}
        </div>
        <div className="flex items-end gap-2">
          <label className="min-w-0 flex-1">
            <span className="sr-only">Message</span>
            <textarea
              aria-label="Message"
              className="max-h-32 min-h-20 w-full resize-none rounded-md border border-[#bfc1b7] bg-[#fdfdf8] px-3 py-2 text-sm text-[#23251d] outline-none transition-colors placeholder:text-[#65675e] hover:border-[#f54e00] focus:border-[#1e1f23] disabled:cursor-not-allowed disabled:bg-[#eeefe9]"
              disabled={!notebookPath || !selectedAgentName || submitting}
              onChange={(event) => setPrompt(event.currentTarget.value)}
              onKeyDown={submitPromptOnEnter}
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
            className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-md border border-[#1e1f23] bg-[#1e1f23] text-[#fdfdf8] transition-colors hover:border-[#f54e00] hover:bg-[#f54e00] disabled:cursor-not-allowed disabled:border-[#bfc1b7] disabled:bg-[#eeefe9] disabled:text-[#65675e]"
            disabled={!canSubmitPrompt}
            title="Send message"
            type="submit"
          >
            <SendIcon size={16} strokeWidth={1.5} />
          </button>
        </div>
      </form>
    </div>
  );
}
