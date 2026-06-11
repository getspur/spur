import { Channel, invoke } from "@tauri-apps/api/core";
import clsx from "clsx";
import { BotIcon, SendIcon } from "lucide-react";
import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import {
  type ChatEvent,
  type ChatMessage,
  DEFAULT_CHAT_APP_KEY,
  useChat,
} from "@/stores/chat";
import { useNotebook } from "@/stores/notebook";

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

export default function ChatPanel() {
  const notebook = useNotebook();
  const [notebookPath, appOpenInfo] = useStore(
    notebook.store,
    useShallow((state) => [state.viewState.path, state.viewState.appOpenInfo]),
  );
  const [prompt, setPrompt] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const [scopeLabel, messages, streaming, streamingText, pendingPermission] =
    useChat(
      useShallow((state) => [
        state.scopeLabel,
        state.messages,
        state.streaming,
        state.streamingText,
        state.pendingPermission,
      ]),
    );
  const setScope = useChat((state) => state.setScope);
  const applyEvent = useChat((state) => state.applyEvent);
  const applyEventToApp = useChat((state) => state.applyEventToApp);
  const clearPendingPermission = useChat(
    (state) => state.clearPendingPermission,
  );

  useEffect(() => {
    if (!notebookPath) {
      setScope(DEFAULT_CHAT_APP_KEY, "Notebook");
      return;
    }

    const appKey = appOpenInfo?.app_root ?? DEFAULT_CHAT_APP_KEY;
    const nextScopeLabel =
      appOpenInfo?.app_name ?? scopeLabelForPath(notebookPath);
    setScope(appKey, nextScopeLabel);

    let disposed = false;
    void invoke<string>("chat_new_session", { notebookPath })
      .then((sessionId) => {
        if (disposed) return;
        return invoke("chat_switch_session", { notebookPath, sessionId });
      })
      .catch((error) => {
        if (!disposed) {
          applyEventToApp(appKey, {
            type: "error",
            message: error instanceof Error ? error.message : String(error),
          });
        }
      });

    return () => {
      disposed = true;
    };
  }, [
    appOpenInfo?.app_name,
    appOpenInfo?.app_root,
    applyEventToApp,
    notebookPath,
    setScope,
  ]);

  const sendPrompt = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const trimmedPrompt = prompt.trim();
    if (!trimmedPrompt || !notebookPath || submitting) return;

    const appKey = appOpenInfo?.app_root ?? DEFAULT_CHAT_APP_KEY;
    const onEvent = new Channel<ChatEvent>();
    onEvent.onmessage = (message) => {
      useChat.getState().applyEventToApp(appKey, message);
    };

    setPrompt("");
    setSubmitting(true);
    try {
      await invoke("chat_turn", {
        notebookPath,
        prompt: trimmedPrompt,
        onEvent,
      });
    } catch (error) {
      applyEventToApp(appKey, {
        type: "error",
        message: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setSubmitting(false);
    }
  };

  const respondToPermission = async (requestId: string, optionId: string) => {
    try {
      await invoke("chat_permission_respond", {
        requestId,
        optionId,
      });
      clearPendingPermission(requestId);
    } catch (error) {
      applyEvent({
        type: "error",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col gap-3 px-3 pb-16 pt-3 text-gray-700">
      <header className="flex items-center gap-2 rounded border border-gray-200 bg-white px-3 py-2">
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
            disabled={!notebookPath || submitting}
            onChange={(event) => setPrompt(event.currentTarget.value)}
            placeholder={
              notebookPath ? "Message the agent" : "Save the notebook to chat"
            }
            value={prompt}
          />
        </label>
        <button
          aria-label="Send message"
          className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded border border-gray-300 bg-white text-gray-600 transition-colors hover:border-gray-900 hover:text-gray-950 disabled:cursor-not-allowed disabled:border-gray-200 disabled:text-gray-300"
          disabled={!notebookPath || !prompt.trim() || submitting || streaming}
          title="Send message"
          type="submit"
        >
          <SendIcon size={16} strokeWidth={1.5} />
        </button>
      </form>
    </div>
  );
}
