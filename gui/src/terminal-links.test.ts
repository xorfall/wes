import { describe, expect, it, vi } from "vitest";
import { findWebLinks, hyperlinkHandler, linksOnRow, openWebAddress, wantsLink, webAddress } from "./terminal-links";

type FakeCell = { chars: string; width: number };
/** A buffer of rows; each row is its cells, and `wrapped` marks a row continuing the one above. */
function buffer(rows: { cells: FakeCell[]; wrapped?: boolean }[]) {
  return {
    getLine: (y: number) => {
      const row = rows[y];
      if (!row) return undefined;
      return {
        isWrapped: row.wrapped ?? false,
        length: row.cells.length,
        getCell: (x: number) => {
          const cell = row.cells[x];
          return cell && { getChars: () => cell.chars, getWidth: () => cell.width };
        },
      };
    },
  } as never;
}
const text = (value: string, width = value.length): FakeCell[] =>
  Array.from({ length: width }, (_, i) => ({ chars: value[i] ?? "", width: 1 }));
const cmd = { metaKey: true, ctrlKey: false } as MouseEvent;
const plain = { metaKey: false, ctrlKey: false } as MouseEvent;

describe("web addresses in terminal text", () => {
  it("should_FindHttpAndHttpsAddresses_When_TheyAppearInOutput", () => {
    // Arrange
    const line = "docs: https://orders-api.staging.internal/docs and http://127.0.0.1:8099/#open/id7";

    // Act
    const found = findWebLinks(line);

    // Assert
    expect(found.map(it => it.url)).toEqual(["https://orders-api.staging.internal/docs", "http://127.0.0.1:8099/#open/id7"]);
    expect(line.slice(found[0]!.start, found[0]!.end)).toBe("https://orders-api.staging.internal/docs");
  });

  it("should_LeaveSentencePunctuationOut_When_AnAddressEndsASentence", () => {
    expect(findWebLinks("See https://example.test/a.").map(it => it.url)).toEqual(["https://example.test/a"]);
    expect(findWebLinks("(https://example.test/b), then").map(it => it.url)).toEqual(["https://example.test/b"]);
  });

  it("should_RefuseAddresses_When_TheyAreNotPlainWebAddresses", () => {
    for (const refused of ["javascript:alert(1)", "file:///etc/hosts", "https://user:secret@example.test/", "ssh://host", "https://"]) {
      expect(webAddress(refused), refused).toBeUndefined();
    }
    expect(findWebLinks("ftp://example.test file:///tmp/x")).toEqual([]);
  });
});

describe("links on a terminal row", () => {
  it("should_SpanBothRows_When_AnAddressWrapsOntoTheNextRow", () => {
    // Arrange: a 20-column terminal, the address continuing on a wrapped row.
    const rows = buffer([
      { cells: text("go https://example.t", 20) },
      { cells: text("est/a b", 20), wrapped: true },
    ]);

    // Act
    const onFirst = linksOnRow(rows, 1), onSecond = linksOnRow(rows, 2);

    // Assert
    expect(onFirst.map(it => it.text)).toEqual(["https://example.test/a"]);
    expect(onFirst[0]!.range).toEqual({ start: { x: 4, y: 1 }, end: { x: 5, y: 2 } });
    expect(onSecond.map(it => it.text)).toEqual(["https://example.test/a"]);
  });

  it("should_PlaceTheLinkInTheRightCells_When_WideCharactersComeFirst", () => {
    // Arrange: two wide characters take four cells before the address.
    const cells: FakeCell[] = [
      { chars: "界", width: 2 }, { chars: "", width: 0 }, { chars: "面", width: 2 }, { chars: "", width: 0 },
      ...text(" https://example.test/x", 23),
    ];

    // Act
    const [link] = linksOnRow(buffer([{ cells }]), 1);

    // Assert
    expect(link!.text).toBe("https://example.test/x");
    expect(link!.range.start).toEqual({ x: 6, y: 1 });
  });

  it("should_OpenTheAddress_When_ClickedWithCmdButNotWithAPlainClick", () => {
    // Arrange
    const open = vi.fn();
    const [link] = linksOnRow(buffer([{ cells: text("https://example.test/") }]), 1, open);

    // Act
    link!.activate(plain, link!.text);
    link!.activate(cmd, link!.text);

    // Assert
    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith("https://example.test/", "_blank", "noopener,noreferrer");
  });
});

describe("hyperlinks a program marks itself", () => {
  it("should_OpenOnlyWebAddresses_When_ActivatedWithAModifier", () => {
    // Arrange
    const open = vi.fn();
    const handler = hyperlinkHandler(open);

    // Act
    handler.activate(cmd, "https://example.test/report", undefined as never);
    handler.activate(cmd, "file:///etc/passwd", undefined as never);
    handler.activate(plain, "https://example.test/other", undefined as never);

    // Assert
    expect(handler.allowNonHttpProtocols).toBe(false);
    expect(open.mock.calls.map(call => call[0])).toEqual(["https://example.test/report"]);
  });

  it("should_TreatCtrlLikeCmd_When_NotOnAMac", () => {
    expect(wantsLink({ metaKey: false, ctrlKey: true })).toBe(true);
    expect(wantsLink(plain)).toBe(false);
    const open = vi.fn();
    openWebAddress("not an address", open);
    expect(open).not.toHaveBeenCalled();
  });
});
