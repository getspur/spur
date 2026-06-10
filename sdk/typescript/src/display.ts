/**
 * `display` — `Symbol.for("Jupyter.display")` helpers.
 *
 * Each helper returns an object whose `[Symbol.for("Jupyter.display")]` method
 * returns the MIME bundle for the given media type. A Deno-Jupyter kernel cell
 * can `return` or use a last-expression with these objects to render rich output.
 *
 * @example
 * ```ts
 * import { display } from "@spur/app";
 *
 * return display.html("<b>Hello</b>");
 * return display.markdown("# Title\nParagraph.");
 * return display.json({ key: "value" });
 * ```
 */

/**
 * A value that Deno-Jupyter can render as rich output.
 * The well-known symbol `Symbol.for("Jupyter.display")` is used as the key
 * so that Deno-Jupyter recognizes it as a display-protocol object.
 */
export type JupyterDisplay = Record<symbol, () => Record<string, unknown>>;

const DISPLAY_SYMBOL = Symbol.for("Jupyter.display");

function makeDisplay(bundle: () => Record<string, unknown>): JupyterDisplay {
  return { [DISPLAY_SYMBOL]: bundle };
}

/**
 * Return a display object that renders `content` as `text/html` in Jupyter.
 */
function html(content: string): JupyterDisplay {
  return makeDisplay(() => ({ "text/html": content }));
}

/**
 * Return a display object that renders `content` as `text/markdown` in Jupyter.
 */
function markdown(content: string): JupyterDisplay {
  return makeDisplay(() => ({ "text/markdown": content }));
}

/**
 * Return a display object that renders `value` as `application/json` in Jupyter.
 * The value is passed through as-is (not stringified) so that the kernel's
 * display system can serialize it appropriately.
 */
function json(value: unknown): JupyterDisplay {
  return makeDisplay(() => ({ "application/json": value }));
}

export const display = { html, markdown, json };
