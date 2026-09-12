import type { HeaderStyler } from "./header.ts";

const PIXELS = [
	"               #########                ",
	"            ###############             ",
	"            ################            ",
	"    ###### ########**######## ######    ",
	"   ################**###############    ",
	" ################******###############  ",
	"###############**********############## ",
	"##################****################# ",
	"###################**################## ",
	" #####################################  ",
	"   ##################################   ",
	"       ##########################       ",
];

const QUADRANTS = [" ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█"];

export const LOGO_WIDTH = 20;

export function renderLogo(styler: Pick<HeaderStyler, "fg">, monochrome = false): string[] {
	const lines: string[] = [];
	for (let y = 0; y < PIXELS.length; y += 2) {
		let line = "";
		for (let x = 0; x < LOGO_WIDTH; x += 1) {
			const pixels = [PIXELS[y]![x * 2], PIXELS[y]![x * 2 + 1], PIXELS[y + 1]![x * 2], PIXELS[y + 1]![x * 2 + 1]];
			const body = pixels.reduce((bits, pixel, index) => bits | (pixel === "#" ? 1 << index : 0), 0);
			line += QUADRANTS[body];
		}
		lines.push(monochrome ? line : styler.fg("accent", line));
	}
	return lines;
}
