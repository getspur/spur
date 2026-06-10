/**
 * @spur/app — Deno/TypeScript frontend SDK for SPUR notebook apps.
 *
 * @example
 * ```ts
 * import { callTool, display, capture, ports } from "@spur/app";
 *
 * // Call a notebook MCP tool
 * const result = await callTool("html_video_render", {
 *   port_names: ["spur-ad-capture"],
 *   output_path: "spur-ad.mp4",
 *   fps: 30,
 * });
 *
 * // Display rich output
 * display.html(`<video controls src="..." />`);
 *
 * // Emit a capture canvas
 * const html = capture.canvas({ port: "my-cell-id", fps: 30, durationSec: 5 });
 *
 * // Read a port
 * const data = await ports.read("spur-ad-capture");
 * ```
 */

export { callTool, callToolWithSocket } from "./src/call_tool.ts";
export type { CallToolOptions, ConnectFn } from "./src/call_tool.ts";

export { capture } from "./src/capture.ts";
export type { CaptureCanvasOptions } from "./src/capture.ts";

export { display } from "./src/display.ts";
export type { JupyterDisplay } from "./src/display.ts";

export { ports } from "./src/ports.ts";
export type { PortData, PortEntry, PortManifest } from "./src/ports.ts";
