import { beforeEach, describe, expect, test, vi } from "vitest";

import {
  daemonControl,
  pathFromDaemonControlResponse,
  recentEntriesFromDaemonControlResponse,
} from "./control";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

describe("daemon control adapter", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test("opens notebooks through the typed daemon_control envelope", async () => {
    invokeMock.mockResolvedValueOnce({ ok: true, path: "/tmp/demo.ipynb" });

    const response = await daemonControl({
      command: "open",
      path: "/tmp/demo.ipynb",
    });

    expect(pathFromDaemonControlResponse(response, "open")).toBe(
      "/tmp/demo.ipynb",
    );
    expect(invokeMock).toHaveBeenCalledWith("daemon_control", {
      cmd: { command: "open", path: "/tmp/demo.ipynb" },
    });
  });

  test("maps enriched list_recents responses to HomePage entries", () => {
    const entries = recentEntriesFromDaemonControlResponse({
      ok: true,
      entries: [
        {
          path: "/tmp/demo.ipynb",
          lastOpened: "2026-05-28T12:00:00Z",
          isScratch: false,
          pinned: true,
          kernelAlive: true,
          isCurrent: true,
        },
      ],
    });

    expect(entries).toEqual([
      {
        path: "/tmp/demo.ipynb",
        lastOpened: "2026-05-28T12:00:00Z",
        isScratch: false,
        pinned: true,
        kernelAlive: true,
        isCurrent: true,
      },
    ]);
  });

  test("requires path responses for path-returning commands", async () => {
    expect(() => pathFromDaemonControlResponse({ ok: true }, "open")).toThrow(
      "daemon open response did not include path",
    );
  });

  test("sends void commands through daemon_control and discards the response", async () => {
    invokeMock.mockResolvedValueOnce({ ok: true });

    await expect(
      daemonControl({
        command: "set_pinned",
        path: "/tmp/demo.ipynb",
        pinned: true,
      }),
    ).resolves.toEqual({ ok: true });

    expect(invokeMock).toHaveBeenCalledWith("daemon_control", {
      cmd: { command: "set_pinned", path: "/tmp/demo.ipynb", pinned: true },
    });
  });
});
