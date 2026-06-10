/**
 * `capture` — helpers for emitting the `data-capture` canvas contract consumed
 * by `withVideoCapture` in jute-notebook/src/ui/notebook/rendering.ts.
 *
 * The host appends the MediaRecorder script to any HTML output that contains
 * `data-capture`. The recorder selects `canvas[data-capture="true"]`, reads
 * `canvas.dataset.captureFps` (default 30) and
 * `canvas.dataset.captureDurationSec ?? canvas.dataset.captureDuration`
 * (default 3), records `canvas.captureStream(fps)` for `durationSec` seconds,
 * then posts:
 *
 *   { type: "jute-video-capture",
 *     cellId: window.name || canvas.dataset.captureCellId || "",
 *     webm,
 *     duration_sec }
 *
 * to `window.parent`. The frontend handler (OutputView.tsx) routes it to the
 * Tauri `push_capture_port` command using `event.data.cellId` as the port name.
 *
 * Therefore `capture.canvas({ port })` must emit `data-capture-cell-id="${port}"`.
 */

/** Options for `capture.canvas`. */
export interface CaptureCanvasOptions {
  /**
   * Port name (= cell ID) written to `data-capture-cell-id`.
   * The host's postMessage handler passes this value directly to
   * `push_capture_port` as the port name.
   */
  port: string;
  /** Capture frame rate in frames per second. Default: 30. */
  fps?: number;
  /** Recording duration in seconds. Default: 3. */
  durationSec?: number;
  /** Canvas width in CSS pixels. Omitted if not provided. */
  width?: number;
  /** Canvas height in CSS pixels. Omitted if not provided. */
  height?: number;
}

/** Escape a value for use as an HTML attribute value (double-quoted). */
function escapeAttr(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Return an HTML string for a `<canvas>` element bearing the `data-capture`
 * attributes that trigger the host's video-capture recorder.
 *
 * @example
 * ```ts
 * import { capture } from "@spur/app";
 *
 * const html = capture.canvas({ port: "my-cell-id", fps: 30, durationSec: 5, width: 1280, height: 720 });
 * // Returns:
 * // <canvas data-capture="true" data-capture-cell-id="my-cell-id"
 * //         data-capture-fps="30" data-capture-duration-sec="5"
 * //         width="1280" height="720"></canvas>
 * ```
 */
function canvas(opts: CaptureCanvasOptions): string {
  const fps = opts.fps ?? 30;
  const durationSec = opts.durationSec ?? 3;
  const dimAttrs = [
    opts.width !== undefined ? ` width="${opts.width}"` : "",
    opts.height !== undefined ? ` height="${opts.height}"` : "",
  ].join("");

  return (
    `<canvas` +
    ` data-capture="true"` +
    ` data-capture-cell-id="${escapeAttr(opts.port)}"` +
    ` data-capture-fps="${fps}"` +
    ` data-capture-duration-sec="${durationSec}"` +
    dimAttrs +
    `></canvas>`
  );
}

export const capture = { canvas };
