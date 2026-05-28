import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import BulletsLayout from "./BulletsLayout";

function renderBullets(markdown: string) {
  return render(
    <BulletsLayout
      blocks={[{ kind: "markdown", md: markdown }]}
      themeId="minimal-light"
    />,
  );
}

describe("BulletsLayout", () => {
  it("renders a single backtick span as code", () => {
    renderBullets("- `foo`");

    expect(screen.getByText("foo").tagName).toBe("CODE");
  });

  it("renders mixed bold and backtick spans", () => {
    renderBullets("- **bold** and `code`");

    expect(screen.getByText("bold").tagName).toBe("B");
    expect(screen.getByText("code").tagName).toBe("CODE");
  });

  it("escapes HTML before rendering backtick spans", () => {
    const { container } = renderBullets("- `<script>`");

    expect(container.querySelector("script")).not.toBeInTheDocument();
    const code = container.querySelector("code");
    expect(code).toHaveTextContent("<script>");
  });
});
