// @vitest-environment jsdom
// AirmanPrep re-skin (f13): the active-target chip, the design credit in
// About, the flag chips and the brand mark. Behaviour only; colours are
// covered by tokens.test.ts.
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { App, activePreset } from "../../src/App";
import { AboutDialog } from "../../src/components/about/AboutDialog";
import { FlagBadges } from "../../src/components/metadata/FlagBadges";
import { PRESET_TARGETS } from "../../src/lib/presets";
import { makeMockFramesResponse } from "../../src/lib/mock";

function stubMatchMedia(): void {
  window.matchMedia = ((query: string) => ({
    matches: true,
    media: query,
    onchange: null,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

afterEach(() => {
  cleanup();
  window.history.replaceState(null, "", "/");
});

describe("activePreset", () => {
  it("matches every preset by its exact strings", () => {
    for (const p of PRESET_TARGETS) {
      expect(activePreset(p.ra, p.dec)?.id).toBe(p.id);
    }
  });

  it("matches a decimal-degree spelling of the same position", () => {
    // Barnard's Star, epoch 2026.0: 269.4462583, +4.7683250.
    expect(activePreset("269.44626", "4.76832")?.id).toBe("barnards-star");
  });

  it("does not match a nearby field (more than 2 arcsec away)", () => {
    expect(activePreset("133.795", "-7.245")).toBeNull();
    expect(activePreset("", "")).toBeNull();
  });
});

describe("active target chip", () => {
  it("names the preset with its epoch when a preset is viewed", () => {
    stubMatchMedia();
    const p = PRESET_TARGETS[0];
    expect(p).toBeDefined();
    if (p === undefined) return;
    window.history.replaceState(
      null,
      "",
      `/?ra=${encodeURIComponent(p.ra)}&dec=${encodeURIComponent(p.dec)}&fov=0.1`,
    );
    render(<App mockData={makeMockFramesResponse()} />);
    expect(
      screen.getByText(`${p.label.toUpperCase()} · EPOCH 2026.0`),
    ).toBeTruthy();
    expect(screen.getByText("Active target:")).toBeTruthy();
  });

  it("is absent for an arbitrary position", () => {
    stubMatchMedia();
    window.history.replaceState(null, "", "/?ra=133.795&dec=-7.245&fov=0.05");
    render(<App mockData={makeMockFramesResponse()} />);
    expect(screen.queryByText(/EPOCH 2026\.0/)).toBeNull();
  });
});

describe("About design credit", () => {
  it("links to AirmanPrep in a new tab with rel=noopener", () => {
    render(<AboutDialog />);
    fireEvent.click(screen.getByRole("button", { name: "About" }));
    const a = screen.getByRole("link", { name: "AirmanPrep" });
    expect(a.getAttribute("href")).toBe("https://airmanprep.com/");
    expect(a.getAttribute("rel")).toContain("noopener");
    expect(a.closest("p")?.textContent).toBe("Design: AirmanPrep");
  });
});

describe("flag chips", () => {
  it("render as warn pill chips with the API flag names", () => {
    render(<FlagBadges flags={["TRANSIENT", "PERSIST"]} />);
    const items = screen.getAllByRole("listitem");
    expect(items.map((i) => i.textContent)).toEqual(["TRANSIENT", "PERSIST"]);
    for (const i of items) {
      expect(i.classList.contains("chip")).toBe(true);
      expect(i.classList.contains("chip-warn")).toBe(true);
    }
  });
});
