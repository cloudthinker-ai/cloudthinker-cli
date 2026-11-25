import { decodeKittyPrintable, matchesKey } from "@earendil-works/pi-tui";

type SearchTheme = { fg(color: string, text: string): string };

export type SearchKey = { kind: "cancel" } | { kind: "confirm" } | { kind: "query"; query: string } | undefined;

export function searchPrompt(theme: SearchTheme, query: string, lead: string): string {
	return `${lead}${theme.fg("accent", "/")}${query}${theme.fg("accent", "▏")}  ${theme.fg("dim", "enter find · esc cancel")}`;
}

export function searchStatus(theme: SearchTheme, query: string, count: number, current: number, suffix: string): string {
	return theme.fg(count > 0 ? "text" : "warning", `"${query}" ${current >= 0 ? `${current + 1}/` : ""}${count} · ${suffix}`);
}

export function stepMatch(current: number, count: number, direction: 1 | -1): number {
	return (current + direction + count) % count;
}

export function printableKey(data: string): string | undefined {
	return decodeKittyPrintable(data) ?? (data.length === 1 && data >= " " ? data : undefined);
}

export function handleSearchKey(query: string, data: string): SearchKey {
	if (matchesKey(data, "escape")) return { kind: "cancel" };
	if (matchesKey(data, "enter") || matchesKey(data, "return")) return { kind: "confirm" };
	if (matchesKey(data, "backspace")) return { kind: "query", query: query.slice(0, -1) };
	const printable = printableKey(data);
	return printable ? { kind: "query", query: query + printable } : undefined;
}
