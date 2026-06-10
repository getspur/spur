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

// Declare as a module-level const so TypeScript can use `typeof DISPLAY_SYMBOL`
// as a unique-symbol-like key in the JupyterDisplay type, giving callers precise
// typing without needing a cast.
const DISPLAY_SYMBOL: unique symbol = Symbol.for(
  "Jupyter.display",
) as unknown as typeof DISPLAY_SYMBOL;

/**
 * A value that Deno-Jupyter can render as rich output.
 *
 * The key is typed as `typeof DISPLAY_SYMBOL` (the module-level const whose
 * runtime value is `Symbol.for("Jupyter.display")`) so callers can access the
 * method directly without a cast:
 *
 * ```ts
 * import { display, DISPLAY_SYMBOL } from "@spur/app/src/display.ts";
 * const bundle = obj[DISPLAY_SYMBOL]();
 * ```
 */
export type JupyterDisplay = {
  readonly [K in typeof DISPLAY_SYMBOL]: () => Record<string, unknown>;
};

/** The well-known symbol used as the Jupyter display key. */
export { DISPLAY_SYMBOL };

function makeDisplay(bundle: () => Record<string, unknown>): JupyterDisplay {
  return { [DISPLAY_SYMBOL]: bundle } as JupyterDisplay;
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
