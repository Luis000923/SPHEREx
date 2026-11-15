// @vitest-environment jsdom
// The header GitHub link points at the project repository and opens safely
// in a new tab.
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { GitHubLink } from "../../src/components/about/GitHubLink";

afterEach(cleanup);

describe("GitHubLink", () => {
  it("links to the repository in a new tab with noopener", () => {
    render(<GitHubLink />);
    const a = screen.getByRole("link", { name: /github/i });
    expect(a.getAttribute("href")).toBe("https://github.com/Luis000923/SPHEREx");
    expect(a.getAttribute("target")).toBe("_blank");
    expect(a.getAttribute("rel")).toContain("noopener");
    expect(a.textContent).toContain("GitHub");
  });
});
