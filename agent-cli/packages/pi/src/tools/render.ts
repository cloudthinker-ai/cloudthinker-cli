import { keyHint, truncateToVisualLines } from "@earendil-works/pi-coding-agent";
import type { AgentToolResult, Theme } from "@earendil-works/pi-coding-agent";
import { Container, Text, hyperlink, truncateToWidth } from "@earendil-works/pi-tui";
import type { Component } from "@earendil-works/pi-tui";

import { CLOUD_TAG, type Legend } from "../awareness.ts";

const PREVIEW_LINES = 5;
const NO_PREVIEW = 0;

export function outputPreviewLines(isError: boolean): number {
	return isError ? PREVIEW_LINES : NO_PREVIEW;
}

export interface Elapsed {
	elapsed_ms?: number;
}

export function firstLine(value: string): string {
	const line = value.split("\n", 1)[0] ?? "";
	return line.trim();
}

export function formatElapsed(elapsedMs: number | undefined): string {
	return `${((elapsedMs ?? 0) / 1000).toFixed(1)}s`;
}

export function callLine(theme: Theme, name: string, ...rest: string[]): string {
	const head = `${theme.fg("accent", CLOUD_TAG)} ${theme.fg("toolTitle", theme.bold(name))}`;
	const tail = rest
		.filter((part) => part.length > 0)
		.map((part) => theme.fg("muted", part))
		.join(theme.fg("muted", "  "));
	return tail.length > 0 ? `${head}  ${tail}` : head;
}

export function legendLine(theme: Theme, toolCallId: string | undefined, legend: Legend): string | undefined {
	const tag = legend.tag(toolCallId);
	return tag ? theme.fg("muted", tag) : undefined;
}

export function callComponent(line: string, expandedDetail?: string, legend?: string): Component {
	const container = new Container();
	if (legend) container.addChild(new Text(legend, 0, 0));
	container.addChild({
		render: (width) => [truncateToWidth(line, width, "…")],
		invalidate: () => {},
	});
	if (expandedDetail) container.addChild(new Text(expandedDetail, 0, 0));
	return container;
}

export function scriptDetail(theme: Theme, script: string, expanded: boolean): string | undefined {
	const trimmed = script.trim();
	return expanded && trimmed.length > 0 ? theme.fg("muted", trimmed) : undefined;
}

export function resultBody(result: AgentToolResult<unknown>): string {
	return result.content
		.map((block) => (block.type === "text" ? block.text : ""))
		.join("\n")
		.trim();
}

function expandHint(theme: Theme, count: string): string {
	return theme.fg("muted", `(${count},`) + ` ${keyHint("app.tools.expand", "to expand")}${theme.fg("muted", ")")}`;
}

export function summaryComponent(
	theme: Theme,
	summary: string,
	body: string,
	expanded: boolean,
	previewLines: number = PREVIEW_LINES,
): Component {
	const container = new Container();
	container.addChild(new Text(theme.fg("toolTitle", summary), 0, 0));
	if (body.length === 0) return container;
	if (!expanded && previewLines === NO_PREVIEW) {
		const lineCount = body.split("\n").length;
		const hint = expandHint(theme, `${lineCount} ${lineCount === 1 ? "line" : "lines"}`);
		container.addChild({
			render: (width) => [truncateToWidth(hint, width, "...")],
			invalidate: () => {},
		});
		return container;
	}
	const styled = body
		.split("\n")
		.map((line) => theme.fg("toolOutput", line))
		.join("\n");
	if (expanded) {
		container.addChild(new Text(`\n${styled}`, 0, 0));
		return container;
	}
	container.addChild({
		render: (width) => {
			const preview = truncateToVisualLines(styled, previewLines, width);
			if (preview.skippedCount > 0) {
				const hint = theme.fg("muted", "... ") + expandHint(theme, `${preview.skippedCount} earlier lines`);
				return ["", truncateToWidth(hint, width, "..."), ...preview.visualLines];
			}
			return ["", ...preview.visualLines];
		},
		invalidate: () => {},
	});
	return container;
}

export function link(theme: Theme, url: string): string {
	return theme.fg("mdLink", hyperlink(url, url));
}
