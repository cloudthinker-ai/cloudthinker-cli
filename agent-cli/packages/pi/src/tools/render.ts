import { keyHint, truncateToVisualLines } from "@earendil-works/pi-coding-agent";
import type { AgentToolResult, Theme } from "@earendil-works/pi-coding-agent";
import { Container, Text, hyperlink, truncateToWidth } from "@earendil-works/pi-tui";
import type { Component } from "@earendil-works/pi-tui";

const PREVIEW_LINES = 5;
export const CLOUD_GLYPH = "☁";

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
	const title = theme.fg("toolTitle", theme.bold(`${CLOUD_GLYPH} ${name}`));
	const tail = rest
		.filter((part) => part.length > 0)
		.map((part) => theme.fg("muted", part))
		.join(theme.fg("muted", "  "));
	return tail.length > 0 ? `${title}  ${tail}` : title;
}

export function callComponent(line: string, expandedDetail?: string): Component {
	const container = new Container();
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

export function summaryComponent(
	theme: Theme,
	summary: string,
	body: string,
	expanded: boolean,
): Component {
	const container = new Container();
	container.addChild(new Text(theme.fg("toolTitle", summary), 0, 0));
	if (body.length === 0) return container;
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
			const preview = truncateToVisualLines(styled, PREVIEW_LINES, width);
			if (preview.skippedCount > 0) {
				const hint =
					theme.fg("muted", `... (${preview.skippedCount} earlier lines,`) +
					` ${keyHint("app.tools.expand", "to expand")}${theme.fg("muted", ")")}`;
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
