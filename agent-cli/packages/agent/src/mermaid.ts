import { renderMermaidTerminal, colorizeTerminalText } from "beautiful-mermaid";
import { Marked, visibleWidth } from "@earendil-works/pi-tui";
import { InteractiveMode, getMarkdownTheme, type MarkdownTransformer } from "@earendil-works/pi-coding-agent";
import type { MarkdownTheme } from "@earendil-works/pi-tui";

type MermaidReason = "unsupported" | "invalid" | "too-wide" | "limit";

function colorsDisabled(): boolean {
    return process.env.NO_COLOR !== undefined || process.env.TERM === "dumb";
}
type MermaidPreview = { kind: "diagram"; lines: string[] } | { kind: "source"; reason: MermaidReason };

export function hasClosingMermaidFence(raw: string) {
    const lines = raw.trimEnd().split("\n");
    const opening = /^ {0,3}(`{3,}|~{3,})/.exec(lines[0] ?? "")?.[1];
    const closing = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(lines.at(-1) ?? "")?.[1];
    return Boolean(lines.length > 1 && opening && closing && opening[0] === closing[0] && closing.length >= opening.length);
}

export function renderMermaidPreview(source: string, width: number, theme: MarkdownTheme, borderColors = false): MermaidPreview {
    if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]|[<>]|%%\{|^\s*(?:subgraph|end|click|linkStyle|direction|---)/m.test(source.replace(/<-\.->|<==>|<-->|-\.->|==>|-->/g, ""))) {
        return { kind: "source", reason: "unsupported" };
    }
    if (!/^\s*(?:flowchart|graph)\s+(?:TD|TB|LR|BT|RL)\s*(?:\n|$)/i.test(source)) {
        return { kind: "source", reason: "unsupported" };
    }
    try {
        const noColor = colorsDisabled();
        const lines = renderMermaidTerminal(source, (text, role, style) => {
            if (noColor) return text;
            if (role === "border") return borderColors && style.stroke ? colorizeTerminalText(text, style.stroke) : theme.code(text);
            if (role === "text") return theme.codeBlock(text);
            return theme.codeBlockBorder(text);
        });
        if (lines.some(line => visibleWidth(line) > width)) return { kind: "source", reason: "too-wide" };
        return { kind: "diagram", lines };
    } catch (error) {
        const reason = error instanceof Error && ["limit", "unsupported", "invalid"].includes(error.message) ? error.message as MermaidReason : "invalid";
        return { kind: "source", reason };
    }
}

export function mermaidFallbackNotice(reason: MermaidReason) {
    return {
        unsupported: "Mermaid preview does not support this syntax; showing source.",
        invalid: "Mermaid preview could not parse this diagram; showing source.",
        "too-wide": "Mermaid diagram does not fit; widen the terminal to preview it.",
        limit: "Mermaid diagram exceeds preview limits; showing source.",
    }[reason];
}

const parser = new Marked();

function codeSpan(line: string): string {
    const content = line.trim() ? line : "\u00a0";
    const longest = Math.max(0, ...Array.from(content.matchAll(/`+/g), match => match[0].length));
    const fence = "`".repeat(longest + 1);
    return `${fence} ${content} ${fence}`;
}

export function createMermaidTransformer(getMode: () => "off" | "final" | "streaming", getBorderColors: () => boolean = () => false): MarkdownTransformer {
    return (markdown, context) => {
        const mode = getMode();
        if (mode === "off" || context.messageType === "assistant-thinking" || (context.isStreaming && mode !== "streaming")) return markdown;
        return parser.lexer(markdown).map(token => {
            if (token.type !== "code" || token.lang?.trim().split(/\s+/, 1)[0]?.toLowerCase() !== "mermaid" || !token.text.trim() || !hasClosingMermaidFence(token.raw)) return token.raw;
            const theme = getMarkdownTheme();
            const preview = renderMermaidPreview(token.text, context.availableWidth, theme, getBorderColors());
            if (preview.kind === "source") {
                const notice = mermaidFallbackNotice(preview.reason);
                const styledNotice = colorsDisabled() ? notice : theme.codeBlockBorder(notice);
                return `${token.raw}\n${codeSpan(styledNotice)}  \n`;
            }
            return `${preview.lines.map(codeSpan).join("  \n")}\n`;
        }).join("");
    };
}

interface MermaidHost {
    mermaidMarkdownTransformer: MarkdownTransformer;
    settingsManager: {
        getMermaidRenderingMode(): "off" | "final" | "streaming";
        getGlobalSettings(): { markdown?: { mermaidBorderColors?: boolean } };
        getProjectSettings(): { markdown?: { mermaidBorderColors?: boolean } };
    };
}
interface MermaidPrototype {
    getMarkdownTransformers(this: MermaidHost): MarkdownTransformer[];
}

let applied = false;

export function applyMermaidUi(): void {
    if (applied) return;
    const prototype = InteractiveMode.prototype as unknown as MermaidPrototype;
    const original = prototype.getMarkdownTransformers;
    if (typeof original !== "function") throw new Error("Pi Markdown transformer boundary changed");
    applied = true;
    prototype.getMarkdownTransformers = function () {
        const transformers = original.call(this);
        if (transformers[0] !== this.mermaidMarkdownTransformer || typeof this.settingsManager.getMermaidRenderingMode !== "function") throw new Error("Pi Mermaid transformer boundary changed");
        return [createMermaidTransformer(() => this.settingsManager.getMermaidRenderingMode(), () => {
            const global = this.settingsManager.getGlobalSettings().markdown?.mermaidBorderColors;
            const project = this.settingsManager.getProjectSettings().markdown?.mermaidBorderColors;
            return (project ?? global) === true;
        }), ...transformers.slice(1)];
    };
}
