#!/usr/bin/env node
// Probe codex-acp 0.12.0: speak ACP wire protocol over stdio, capture every
// frame the agent sends so we can confirm what v1 (model/effort) and v2
// (review-branch input + _meta) look like in practice.
//
// Usage:
//   node scripts/probe-codex-acp.mjs            # full handshake + new_session
//   node scripts/probe-codex-acp.mjs --load     # also exercise session/load
//   node scripts/probe-codex-acp.mjs --prompts  # multi-turn session/prompt to
//                                                 capture SessionInfoUpdate
//                                                 (M8 Wave 0.3)
//
// All frames are logged to stderr with a TX/RX prefix and a wallclock stamp.
// stdout is reserved for a final JSON summary.

import { spawn } from "node:child_process";
import { stderr, stdout, exit } from "node:process";

const args = process.argv.slice(2);
const wantLoad = args.includes("--load");
const wantPrompts = args.includes("--prompts");

const DEADLINE_MS = wantPrompts ? 90_000 : 30_000;
const startedAt = Date.now();
let nextId = 1;
const pending = new Map();
const captured = {
    initialize: null,
    new_session: null,
    notifications: [], // every server-initiated frame
    wantedConfigOptions: null,
    wantedAvailableCommands: null,
    sessionInfoUpdates: [], // M8 Wave 0.3
};

const child = spawn("npx", ["--yes", "@zed-industries/codex-acp@0.12.0"], {
    stdio: ["pipe", "pipe", "inherit"],
    env: { ...process.env },
});

child.on("error", (e) => {
    stderr.write(`[probe] spawn error: ${e}\n`);
    exit(2);
});
child.on("exit", (code, sig) => {
    stderr.write(`[probe] codex exit code=${code} sig=${sig}\n`);
});

let buf = "";
child.stdout.on("data", (chunk) => {
    buf += chunk.toString("utf8");
    let nl;
    while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (!line) continue;
        onLine(line);
    }
});

function send(method, params, isNotification = false) {
    const msg = isNotification
        ? { jsonrpc: "2.0", method, params }
        : { jsonrpc: "2.0", id: nextId++, method, params };
    const line = JSON.stringify(msg);
    stamp("TX", line);
    child.stdin.write(line + "\n");
    if (isNotification) return Promise.resolve(null);
    return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
            pending.delete(msg.id);
            reject(new Error(`timeout waiting for ${method}`));
        }, DEADLINE_MS);
        pending.set(msg.id, { resolve, reject, method, timer });
    });
}

function stamp(prefix, payload) {
    const t = ((Date.now() - startedAt) / 1000).toFixed(3);
    stderr.write(`[+${t}s ${prefix}] ${payload}\n`);
}

function onLine(line) {
    stamp("RX", line);
    let msg;
    try {
        msg = JSON.parse(line);
    } catch (e) {
        stderr.write(`[probe] non-json line: ${line}\n`);
        return;
    }
    // Server-initiated request (e.g. fs/read, terminal/create).
    if (msg.method && msg.id != null) {
        // Reply with method-not-found so codex doesn't hang.
        const reply = {
            jsonrpc: "2.0",
            id: msg.id,
            error: { code: -32601, message: "method not implemented in probe" },
        };
        const rl = JSON.stringify(reply);
        stamp("TX", rl);
        child.stdin.write(rl + "\n");
        return;
    }
    // Server-initiated notification (e.g. session/update).
    if (msg.method && msg.id == null) {
        captured.notifications.push(msg);
        if (msg.method === "session/update") {
            const upd = msg.params?.update;
            if (upd && (upd.config_options || upd.configOptions)) {
                captured.wantedConfigOptions = upd;
            }
            if (
                upd &&
                (upd.available_commands || upd.availableCommands ||
                 upd.sessionUpdate === "available_commands_update")
            ) {
                captured.wantedAvailableCommands = upd;
            }
            if (
                upd &&
                (upd.sessionUpdate === "session_info_update" ||
                 upd.session_info_update || upd.sessionInfoUpdate)
            ) {
                captured.sessionInfoUpdates.push(upd);
            }
        }
        return;
    }
    // Response to one of our requests.
    const slot = pending.get(msg.id);
    if (!slot) return;
    pending.delete(msg.id);
    clearTimeout(slot.timer);
    if (msg.error) slot.reject(new Error(`${slot.method} -> ${JSON.stringify(msg.error)}`));
    else slot.resolve(msg.result);
}

async function main() {
    // 1. initialize
    const initRes = await send("initialize", {
        protocolVersion: 1,
        clientCapabilities: {
            fs: { readTextFile: true, writeTextFile: true },
            terminal: true,
        },
    });
    captured.initialize = initRes;

    // 2. authenticate (codex requires this; pick first method advertised, if any)
    const authMethods = initRes?.authMethods || initRes?.auth_methods || [];
    if (authMethods.length > 0) {
        try {
            await send("authenticate", { methodId: authMethods[0].id });
        } catch (e) {
            stderr.write(`[probe] authenticate failed (continuing): ${e.message}\n`);
        }
    }

    // 3. session/new
    let sessionId = null;
    try {
        const newRes = await send("session/new", {
            cwd: process.cwd(),
            mcpServers: [],
        });
        captured.new_session = newRes;
        sessionId = newRes?.sessionId || newRes?.session_id || null;
        stderr.write(`[probe] new_session keys: ${Object.keys(newRes || {}).join(", ")}\n`);
    } catch (e) {
        stderr.write(`[probe] session/new failed: ${e.message}\n`);
    }

    // Give the agent a moment to push delayed notifications.
    await new Promise((r) => setTimeout(r, 1500));

    // 3a. (M8 Wave 0.3) Multi-turn prompts to elicit SessionInfoUpdate.
    // Codex auto-generates a session title after the first prompt; the title
    // is delivered via session/update with sessionUpdate="session_info_update".
    if (wantPrompts && sessionId) {
        const prompts = ["what's 2+2", "and 3+3"];
        for (const text of prompts) {
            try {
                stderr.write(`[probe] prompt: "${text}"\n`);
                await send("session/prompt", {
                    sessionId,
                    prompt: [{ type: "text", text }],
                });
                // Wait for stream to settle + any post-turn notifications.
                await new Promise((r) => setTimeout(r, 2000));
            } catch (e) {
                stderr.write(`[probe] session/prompt failed: ${e.message}\n`);
                break;
            }
        }
        // Grace period for codex to push delayed SessionInfoUpdate.
        await new Promise((r) => setTimeout(r, 2000));
    }

    // 4. Print summary to stdout.
    stdout.write(JSON.stringify({
        initialize: captured.initialize,
        new_session: captured.new_session,
        notification_count: captured.notifications.length,
        notification_methods: captured.notifications.map((n) => ({
            method: n.method,
            sessionUpdate: n.params?.update?.sessionUpdate,
            keys: Object.keys(n.params?.update || {}),
        })),
        wantedConfigOptions: captured.wantedConfigOptions,
        wantedAvailableCommands: captured.wantedAvailableCommands,
        sessionInfoUpdates: captured.sessionInfoUpdates,
        all_notifications: captured.notifications,
    }, null, 2) + "\n");

    child.kill("SIGTERM");
    setTimeout(() => process.exit(0), 500);
}

main().catch((e) => {
    stderr.write(`[probe] fatal: ${e.stack || e.message}\n`);
    child.kill("SIGTERM");
    setTimeout(() => process.exit(1), 500);
});
