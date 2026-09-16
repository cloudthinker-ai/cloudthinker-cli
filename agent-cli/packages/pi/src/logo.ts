import type { HeaderStyler } from "./header.ts";

const WORDMARK = [
	"  ██████╗ ██╗       ██████╗  ██╗   ██╗ ██████╗",
	" ██╔════╝ ██║      ██╔═══██╗ ██║   ██║ ██╔══██╗",
	" ██║      ██║      ██║   ██║ ██║   ██║ ██║  ██║",
	" ██║      ██║      ██║   ██║ ██║   ██║ ██║  ██║",
	" ╚██████╗ ███████╗ ╚██████╔╝ ╚██████╔╝ ██████╔╝",
	"  ╚═════╝ ╚══════╝  ╚═════╝   ╚═════╝  ╚═════╝",
	" ████████╗ ██╗  ██╗ ██╗ ███╗   ██╗ ██╗  ██╗ ███████╗ ██████╗",
	" ╚══██╔══╝ ██║  ██║ ██║ ████╗  ██║ ██║ ██╔╝ ██╔════╝ ██╔══██╗",
	"    ██║    ███████║ ██║ ██╔██╗ ██║ █████╔╝  █████╗   ██████╔╝",
	"    ██║    ██╔══██║ ██║ ██║╚██╗██║ ██╔═██╗  ██╔══╝   ██╔══██╗",
	"    ██║    ██║  ██║ ██║ ██║ ╚████║ ██║  ██╗ ███████╗ ██║  ██║",
	"    ╚═╝    ╚═╝  ╚═╝ ╚═╝ ╚═╝  ╚═══╝ ╚═╝  ╚═╝ ╚══════╝ ╚═╝  ╚═╝",
];

type Stops = readonly [readonly [number, number, number], readonly [number, number, number]];

const DARK_STOPS: Stops = [[142, 197, 235], [43, 181, 168]];
const LIGHT_STOPS: Stops = [[26, 111, 158], [0, 120, 111]];

export const LOGO_WIDTH = WORDMARK.reduce((width, row) => Math.max(width, row.length), 0);

const CUBE_LEVELS = [0, 95, 135, 175, 215, 255];

function paletteChannels(index: number): [number, number, number] | undefined {
	if (index >= 232) {
		const gray = 8 + (index - 232) * 10;
		return [gray, gray, gray];
	}
	if (index >= 16) {
		const cell = index - 16;
		return [
			CUBE_LEVELS[Math.floor(cell / 36)]!,
			CUBE_LEVELS[Math.floor((cell % 36) / 6)]!,
			CUBE_LEVELS[cell % 6]!,
		];
	}
	return undefined;
}

function foregroundLuminance(styler: Pick<HeaderStyler, "getFgAnsi">): number | undefined {
	const ansi = styler.getFgAnsi?.("text");
	const truecolor = ansi?.match(/^\x1b\[38;2;(\d+);(\d+);(\d+)m$/);
	const palette = !truecolor ? ansi?.match(/^\x1b\[38;5;(\d+)m$/) : undefined;
	if (!truecolor && !palette) return undefined;
	const channels = truecolor
		? [Number(truecolor[1]!), Number(truecolor[2]!), Number(truecolor[3]!)]
		: paletteChannels(Number(palette![1]!));
	if (!channels) return undefined;
	const channel = (value: number) => {
		const c = value / 255;
		return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
	};
	const [r, g, b] = channels.map(channel);
	return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
}

export function renderLogo(styler: HeaderStyler, monochrome = false): string[] {
	const luminance = foregroundLuminance(styler);
	const [top, bottom] = luminance !== undefined && luminance < 0.5 ? LIGHT_STOPS : DARK_STOPS;
	return WORDMARK.map((row, y) => {
		if (monochrome) return row;
		const t = y / (WORDMARK.length - 1);
		const channel = (i: number) => Math.round(top[i]! + (bottom[i]! - top[i]!) * t);
		return `\x1b[38;2;${channel(0)};${channel(1)};${channel(2)}m${row}\x1b[39m`;
	});
}
