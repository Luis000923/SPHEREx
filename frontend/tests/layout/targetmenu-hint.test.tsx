// @vitest-environment jsdom
// The arrow hint points at the Targets button until the user touches it.
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { TargetMenu } from "../../src/components/layout/TargetMenu";

afterEach(cleanup);

const targets = [
  { id: "a", label: "A", ra: "10", dec: "20", fov: 0.1 },
] as const;

describe("TargetMenu hint arrow", () => {
  it("shows on first render and disappears once the button is touched", () => {
    const { container } = render(
      <TargetMenu targets={targets} onSelect={() => undefined} />,
    );
    expect(container.querySelector(".tmenu-hint")).not.toBeNull();
    const details = container.querySelector("details");
    expect(details).not.toBeNull();
    if (details !== null) fireEvent.pointerDown(details);
    expect(container.querySelector(".tmenu-hint")).toBeNull();
  });

  it("is hidden from assistive tech", () => {
    const { container } = render(
      <TargetMenu targets={targets} onSelect={() => undefined} />,
    );
    expect(
      container.querySelector(".tmenu-hint")?.getAttribute("aria-hidden"),
    ).toBe("true");
  });
});
