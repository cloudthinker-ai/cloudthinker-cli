import assert from "node:assert/strict";
import test from "node:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { stripVTControlCharacters } from "node:util";
import { Markdown, visibleWidth } from "@earendil-works/pi-tui";
import { InteractiveMode, SettingsManager, getMarkdownTheme, initTheme, type MarkdownTransformer } from "@earendil-works/pi-coding-agent";
import { colorizeTerminalText, renderMermaidTerminal } from "beautiful-mermaid";

import { createMermaidTransformer, applyMermaidUi } from "../src/mermaid.ts";

initTheme("dark");
const source = "flowchart LR\nA[Ask] --> B{Ready?}\nB -->|yes| C[Done]\nB -->|no| D[Retry]";
const fence = (body: string, marker = "```") => `${marker}mermaid\n${body}\n${marker}`;
const component = (body: string, padding = 0, mode: "off" | "final" | "streaming" = "streaming", isStreaming = false, borderColors = false) => {
    const transform = createMermaidTransformer(() => mode, () => borderColors);
    return new Markdown(body, padding, 0, getMarkdownTheme(), undefined, {transform: (markdown, availableWidth) => transform(markdown, {messageType: "assistant", isStreaming, availableWidth})});
};
const render = (body: string, width = 140) => component(body).render(width);
const plain = (lines: string[]) => lines.map(stripVTControlCharacters).map(line => line.trimEnd()).join("\n");

test("CA-MMD-1/2/4/6: completed fences preview diagrams without changing streamed or adjacent content", () => {
    const original = (markdown: string) => markdown;
    const extension = (markdown: string) => markdown;
    const host = {mermaidMarkdownTransformer: original, session: {extensionRunner: {getMarkdownTransformers: () => [extension]}}, settingsManager: {getMermaidRenderingMode: () => "final" as const}};
    applyMermaidUi();
    applyMermaidUi();
    const transformers = (InteractiveMode.prototype as unknown as {getMarkdownTransformers(): Array<typeof original>}).getMarkdownTransformers.call(host);
    assert.equal(transformers.length, 2);
    assert.notEqual(transformers[0], original);
    assert.equal(transformers[1], extension);
    const completed = plain(render(`Before\n\n${fence(source)}\n\nAfter`));
    assert.match(completed, /┌|╭/);
    assert.doesNotMatch(completed, /flowchart LR|```mermaid/);
    assert.match(completed, /^Before/);
    assert.match(completed, /After$/);
    const prefix = fence(source).slice(0, -3);
    for (const closer of ["", "`", "``"]) {
        const result = plain(render(prefix + closer));
        assert.match(result, /flowchart LR/);
        assert.doesNotMatch(result, /┌|╭/);
    }
    assert.match(plain(component(fence(source), 0, "streaming", true).render(140)), /┌|╭/);
    assert.match(plain(component(fence(source), 0, "final", true).render(140)), /flowchart LR/);
    assert.match(plain(component(fence(source), 0, "off").render(140)), /flowchart LR/);
    assert.match(plain(render(fence(source, "~~~~"))), /┌|╭/);
    assert.match(plain(render(fence(""))), /```mermaid/);
    assert.match(plain(render("```ts\nconst diagram = 1;\n```")), /const diagram = 1/);
    assert.match(plain(render(`> ${fence("flowchart TD\nA[Start] --> B[End]").replaceAll("\n", "\n> ")}`)), /Start/);
    assert.match(plain(render(fence("flowchart TD\nA[Café] --> B[Done]"))), /Café/);
});

test("CA-MMD-3/5/9/11: source fallback is bounded and reevaluated on resize", () => {
    for (const body of ["sequenceDiagram\nA->>B: Hi", "flowchart TD\nA[Broken", "flowchart TD\nA -->", "flowchart RL\nA --> B", "flowchart TD\nA[中文] --> B", "flowchart TD\nA[Start] --> B\nstyle A stroke-width:4px", "flowchart TD\nA --> B\nclick A href https://example.com"]) {
        const text = plain(render(fence(body)));
        assert.match(text, /showing source/);
        assert.ok(text.includes(body.split("\n")[0]!));
    }
    const diagram = component(fence(source), 1);
    const narrow = diagram.render(24);
    assert.ok(narrow.every(line => visibleWidth(line) <= 24));
    assert.match(plain(narrow), /does\s+not fit/);
    assert.match(plain(diagram.render(140)), /┌|╭/);
    assert.match(plain(render(fence(`flowchart TD\n${Array.from({length: 17}, (_, i) => `N${i}[Node ${i}]`).join("\n")}`))), /exceeds preview limits/);
    assert.match(plain(render(fence(`flowchart TD\nA[${"x".repeat(17000)}]`))), /exceeds preview limits/);
    assert.match(plain(render(fence("flowchart TD\nA[Start] --> B\nstyle A color:javascript:alert(1)"))), /showing source/);
});

test("CA-MMD-7/8: authored stroke colors render on borders while node interiors and text follow the theme", () => {
    const colored = "flowchart LR\nA[Alpha]:::hot --> B[Beta]\nclassDef hot fill:#123456,stroke:#abcdef,color:#fedcba\nstyle B color:#00ff00";
    const spans: Array<{ text: string; role: string | null; style: Record<string, string> }> = [];
    const lines = renderMermaidTerminal(colored, (text, role, style) => {
        spans.push({text, role, style});
        return text;
    });
    assert.match(lines.join("\n"), /Alpha.*Beta/);
    assert.equal(spans.find(span => span.text.includes("Alpha"))?.style.color, "#fedcba");
    assert.equal(spans.find(span => span.text.includes("Beta"))?.style.color, "#00ff00");
    assert.equal(spans.find(span => span.text.includes("Alpha"))?.style.fill, "#123456");
    const previousColor = process.env.NO_COLOR;
    const previousTerm = process.env.TERM;
    const previousColorterm = process.env.COLORTERM;
    const isTtyDescriptor = Object.getOwnPropertyDescriptor(process.stdout, "isTTY");
    try {
        delete process.env.NO_COLOR;
        process.env.TERM = "xterm-256color";
        process.env.COLORTERM = "truecolor";
        Object.defineProperty(process.stdout, "isTTY", {configurable: true, value: true});
        const preview = component(fence(colored), 0, "streaming", false, true).render(140).join("\n");
        const strokePrefix = colorizeTerminalText("x", "#abcdef").split("x", 1)[0]!;
        assert.ok(preview.includes(strokePrefix));
        assert.ok(!render(fence(colored)).join("\n").includes(strokePrefix));
        const root = mkdtempSync(join(tmpdir(), "ct-mermaid-settings-"));
        try {
            const agentDir = join(root, "agent");
            mkdirSync(agentDir);
            mkdirSync(join(root, ".pi"));
            for (const [global, project, enabled] of [
                [undefined, undefined, false], [true, undefined, true],
                [true, false, false], [false, true, true],
            ] as const) {
                writeFileSync(join(agentDir, "settings.json"), JSON.stringify({ markdown: { mermaidBorderColors: global } }));
                writeFileSync(join(root, ".pi", "settings.json"), JSON.stringify({ markdown: { mermaidBorderColors: project } }));
                const original: MarkdownTransformer = markdown => markdown;
                const host = {
                    mermaidMarkdownTransformer: original,
                    session: { extensionRunner: { getMarkdownTransformers: () => [] } },
                    settingsManager: SettingsManager.create(root, agentDir),
                };
                applyMermaidUi();
                const transformers = (InteractiveMode.prototype as unknown as { getMarkdownTransformers(): MarkdownTransformer[] }).getMarkdownTransformers.call(host);
                const output = transformers[0]!(fence(colored), { messageType: "assistant", isStreaming: false, availableWidth: 140 });
                assert.equal(output.includes(strokePrefix), enabled);
            }
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
        assert.doesNotMatch(preview, /\u001b\[48;/);
        for (const authoredInteriorColor of ["#123456", "#fedcba", "#00ff00"]) {
            const interiorPrefix = colorizeTerminalText("x", authoredInteriorColor).split("x", 1)[0]!;
            assert.ok(!preview.includes(interiorPrefix));
        }
    } finally {
        if (previousColor === undefined) delete process.env.NO_COLOR;
        else process.env.NO_COLOR = previousColor;
        if (previousTerm === undefined) delete process.env.TERM;
        else process.env.TERM = previousTerm;
        if (previousColorterm === undefined) delete process.env.COLORTERM;
        else process.env.COLORTERM = previousColorterm;
        if (isTtyDescriptor) Object.defineProperty(process.stdout, "isTTY", isTtyDescriptor);
        else Reflect.deleteProperty(process.stdout, "isTTY");
    }
    const redirectedColor = process.env.NO_COLOR;
    const redirectedTerm = process.env.TERM;
    const redirectedTtyDescriptor = Object.getOwnPropertyDescriptor(process.stdout, "isTTY");
    try {
        delete process.env.NO_COLOR;
        process.env.TERM = "xterm-256color";
        Object.defineProperty(process.stdout, "isTTY", { configurable: true, value: false });
        assert.equal(colorizeTerminalText("redirected", "#abcdef"), "redirected");
    } finally {
        if (redirectedColor === undefined) delete process.env.NO_COLOR;
        else process.env.NO_COLOR = redirectedColor;
        if (redirectedTerm === undefined) delete process.env.TERM;
        else process.env.TERM = redirectedTerm;
        if (redirectedTtyDescriptor) Object.defineProperty(process.stdout, "isTTY", redirectedTtyDescriptor);
        else Reflect.deleteProperty(process.stdout, "isTTY");
    }
    for (const theme of ["light", "dark"]) {
        initTheme(theme);
        assert.match(plain(render(fence(colored))), /Alpha/);
    }
    const previous = process.env.NO_COLOR;
    const noColorTtyDescriptor = Object.getOwnPropertyDescriptor(process.stdout, "isTTY");
    try {
        process.env.NO_COLOR = "1";
        Object.defineProperty(process.stdout, "isTTY", { configurable: true, value: true });
        const rendered = render(fence(colored));
        assert.equal(rendered.join("\n"), stripVTControlCharacters(rendered.join("\n")));
        assert.equal(colorizeTerminalText("label", "#abc"), "label");
        const fallback = component(fence("sequenceDiagram\nA->>B: Hi")).render(140).join("\n");
        const fallbackNotice = fallback.split("\n").find(line => line.includes("Mermaid preview does not support"));
        assert.ok(fallbackNotice);
        assert.equal(fallbackNotice, stripVTControlCharacters(fallbackNotice));
    } finally {
        if (previous === undefined) delete process.env.NO_COLOR;
        else process.env.NO_COLOR = previous;
        if (noColorTtyDescriptor) Object.defineProperty(process.stdout, "isTTY", noColorTtyDescriptor);
        else Reflect.deleteProperty(process.stdout, "isTTY");
        initTheme("dark");
    }
});
