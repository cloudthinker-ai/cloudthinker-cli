import { foregroundAnsi, getTerminalColorMode, rgbColor } from "@earendil-works/pi-tui";
import type { HeaderStyler } from "./header.ts";

type Rgb = readonly [number, number, number];
type Palette = readonly Rgb[];
type Art = { readonly width: number; readonly glyphs: readonly string[]; readonly tones: readonly string[] };

const TONES = "0123456789abcdefghijklmnopqrst";

const DARK_PALETTE: Palette = [
	[87, 112, 137], [83, 109, 134], [84, 110, 135], [87, 112, 137], [84, 109, 135],
	[80, 106, 132], [86, 111, 136], [98, 122, 146], [108, 130, 153], [119, 140, 161],
	[130, 150, 170], [140, 158, 177], [149, 166, 184], [155, 171, 188], [159, 174, 191],
	[164, 179, 195], [171, 185, 200], [175, 189, 204], [179, 192, 206], [183, 196, 209],
	[188, 200, 213], [191, 203, 215], [194, 205, 217], [198, 209, 220], [201, 211, 222],
	[203, 213, 224], [203, 213, 224], [214, 222, 232], [255, 255, 255], [255, 255, 255],
];

const LIGHT_PALETTE: Palette = [
	[109, 125, 143], [111, 127, 145], [110, 126, 144], [109, 125, 143], [110, 126, 144],
	[112, 128, 146], [109, 125, 143], [104, 120, 138], [100, 116, 134], [96, 111, 129],
	[91, 106, 123], [87, 102, 119], [83, 98, 115], [80, 95, 113], [79, 94, 111],
	[76, 91, 108], [74, 89, 105], [72, 86, 103], [70, 85, 102], [68, 83, 100],
	[66, 81, 98], [65, 80, 96], [64, 78, 95], [62, 77, 93], [61, 75, 92],
	[60, 74, 91], [60, 74, 91], [55, 70, 86], [30, 40, 52], [30, 40, 52],
];

const HERO_80: Art = {
	width: 44,
	glyphs: [
		"                ⣀⣤⣶⣶⣿⣿⣿⣿⣷⣶⣤⣀",
		"             ⣀⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⣀",
		"           ⢀⣼⣿⣿⣿⣿⣿⣿⣿⣿⡟⢻⣿⣿⣿⣿⣿⣿⣿⣿⣧⡀",
		"       ⢀⣀⣀⣠⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠁⠈⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣄⣀⣀⡀",
		"   ⢀⣤⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠇  ⠸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣶⣤⡀",
		" ⢀⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟⠁    ⠈⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⡀",
		"⢠⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡟⠛⠋⠉          ⠉⠙⠛⢻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡄",
		"⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣶⣶⣤⣀        ⣀⣤⣶⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷",
		"⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣆    ⣰⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
		"⢻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣇  ⣸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡟",
		"⠈⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡄⢠⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⠁",
		"  ⠙⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⠋",
		"    ⠉⠛⠿⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⠿⠛⠉",
	],
	tones: [
		"                jacfggggfcaj",
		"             jadgjlnopponljgdaj",
		"           j8bfiknpppttpppnkifb8j",
		"       jjjj7adgjlnpppttpppnljgda7jjjj",
		"   jaeikmmmkbdgikmnpt  tpnmkigdbkmmmkieaj",
		" j9dhloqqqqpoleghjtt    ttjhgelopppppolhd9j",
		"j8bfjmoqqqqqqtttt          ttttppppppomjfb8j",
		"28begjlmnoonljtttt        ttttjlnoonmljgeb85",
		"669bdfghiiihgfdb889t    t988bdfghiiihgfcb955",
		"36679abcccccba982222t  t111189abcccccba97555",
		"j03333788888766666222tt22244441788888711111j",
		"  0000000333366666666666222222224444111111",
		"    j0000000000333336666666666622222222j",
	],
};

const HERO_120: Art = {
	width: 64,
	glyphs: [
		"                        ⢀⣀⣤⣴⣶⣾⣿⣿⣿⣿⣷⣶⣦⣤⣀⡀",
		"                     ⢀⣤⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣤⡀",
		"                   ⣠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣄",
		"                 ⣠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣄",
		"                ⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠁⠈⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦",
		"         ⢀⣀⣤⣤⣤⣤⣼⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡏  ⢹⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣧⣤⣤⣤⣤⣀⡀",
		"     ⢀⣤⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿    ⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣶⣤⡀",
		"   ⢀⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟      ⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⡀",
		"  ⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⠛⠁        ⠈⠛⢿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦",
		" ⣼⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟⠛⠋⠉                ⠉⠙⠛⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣧",
		"⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⣤⣄⣀                ⣀⣠⣤⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡇",
		"⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣤⡀        ⢀⣤⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
		"⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦      ⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿",
		"⢻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷    ⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡟",
		"⠘⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣇  ⣸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃",
		" ⠘⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡀⢀⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃",
		"  ⠈⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟⠁",
		"    ⠈⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟⠁",
		"       ⠉⠛⠻⠿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠿⠟⠛⠉",
	],
	tones: [
		"                        jjabceffffecbajj",
		"                     j9beghjkkllllkkjhgeb9j",
		"                   5acfgjkmnopppppponmkjgfca1",
		"                 58bdfhjlnopppppppppponljhfdb85",
		"                58acfhjkmopppppttpppppomkjhfca85",
		"         jjabccb79bdfhjkmnppppt  tppppnmkjhfdb97bccbbjj",
		"     j9cfhjklmmlkjbdfgijlmnopt    tponmljigfdbjklmmlkjhfc9j",
		"   j9cfikmoqpppppomcefgijklmt      tmlkjigfecmoppppppomkifc9j",
		"  1acfikmqqqqqqqqqomcdfghttt        ttthgfdcmopppppppppnkifca5",
		" 29begikmoqqqqqqqqomtttt                ttttmoppppppppomkigeb95",
		"229bdfhjlmnqqqqqonmktttt                ttttkmnopppppnmljhfdb955",
		"668abdfhijklmmmllkjigf99attt        ttta99fgijkllmmmlkjigfdba855",
		"6668abcefghhiiiihhgfdcb78899t      t99887bcdfgghiiiihhgfecba8555",
		"666689abccdeeeeeedcbba9222277t    t7711179abbcddeeeeedcbba985555",
		"j333367899aaabbaaa998722222222t  t444441117899aaabbaaa998711115j",
		" j00333336777887776666666662222tt22222244444417778877711111111j",
		"  j0000000333333336666666666666666222222222224444441111111111j",
		"    j000000000000333333366666666666666662222222222244444411j",
		"       j000000000000000333333366666666666666662222222222j",
	],
};

const MARK: Art = {
	width: 16,
	glyphs: [
		"    ⢀⣴⣾⣿⣿⣷⣦⡀",
		" ⣀⣤⣤⣿⣿⣿⡏⢹⣿⣿⣿⣤⣤⣀",
		"⣼⣿⣿⣿⣿⠛⠋  ⠙⠛⣿⣿⣿⣿⣧",
		"⣿⣿⣿⣿⣿⣿⣷⡄⢠⣾⣿⣿⣿⣿⣿⣿",
		"⠈⠻⣿⣿⣿⣿⣿⣷⣾⣿⣿⣿⣿⣿⠟⠁",
	],
	tones: [
		"    jajqqjaj",
		" jafajrssrjafaj",
		"7hqqrqs  sqrpph7",
		"6aefhjssssjgfda5",
		"j00036eppe22441j",
	],
};

export const LOGO_WIDTH = HERO_80.width;
export const MARK_WIDTH = MARK.width;
export const MARK_HEIGHT = MARK.glyphs.length;

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

function asciiGlyph(glyph: string): string {
	let dots = glyph.codePointAt(0)! - 0x2800;
	let count = 0;
	for (; dots > 0; dots >>= 1) count += dots & 1;
	return count > 4 ? ":" : ".";
}

function ink(r: number, g: number, b: number): string {
	return foregroundAnsi(rgbColor(r, g, b), getTerminalColorMode());
}

function paletteFor(styler: HeaderStyler): Palette {
	const luminance = foregroundLuminance(styler);
	return luminance !== undefined && luminance < 0.5 ? LIGHT_PALETTE : DARK_PALETTE;
}

function peakColor(palette: Palette): Rgb {
	return palette[palette.length - 1]!;
}

function noiseColor(palette: Palette): Rgb {
	return palette === DARK_PALETTE ? [66, 82, 98] : [176, 186, 196];
}

function paint(art: Art, styler: HeaderStyler, monochrome: boolean, ascii: boolean, lit?: number): string[] {
	const palette = paletteFor(styler);
	const peak = peakColor(palette);
	return art.glyphs.map((row, y) => {
		const glyphs = Array.from(row.padEnd(art.width));
		if (monochrome) return glyphs.map((glyph) => glyph === " " || !ascii ? glyph : asciiGlyph(glyph)).join("");
		let output = "";
		let color = "";
		for (const [x, glyph] of glyphs.entries()) {
			if (glyph === " ") {
				output += glyph;
				continue;
			}
			const base = palette[TONES.indexOf(art.tones[y]![x]!)]!;
			const [r, g, b] = lit === y ? base.map((value, i) => Math.round((value + peak[i]!) / 2)) : base;
			const next = ink(r, g, b);
			if (next !== color) {
				output += next;
				color = next;
			}
			output += ascii ? asciiGlyph(glyph) : glyph;
		}
		return output + "\x1b[39m";
	});
}

export function renderLogo(styler: HeaderStyler, monochrome = false, large = false, ascii = false): string[] {
	return paint(large ? HERO_120 : HERO_80, styler, monochrome, ascii);
}

export function renderMark(styler: HeaderStyler, monochrome = false, ascii = false, motionRow?: number): string[] {
	return paint(MARK, styler, monochrome, ascii, motionRow);
}

export const INTRO_FRAMES = 28;

type Dot = { readonly x: number; readonly y: number; readonly bit: number; readonly tone: number; readonly lock: number; readonly hidden: number };

const DOT_BITS: readonly (readonly [number, number])[] = [[0x1, 0x8], [0x2, 0x10], [0x4, 0x20], [0x40, 0x80]];
const DOT_CACHE = new Map<Art, readonly Dot[]>();

function seed(a: number, b: number): number {
	let h = Math.imul(a, 374761393) ^ Math.imul(b + 7, 668265263);
	h = Math.imul(h ^ (h >>> 13), 1274126177);
	return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
}

function blend(from: Rgb, to: Rgb, amount: number): Rgb {
	return [
		Math.round(from[0] + (to[0] - from[0]) * amount),
		Math.round(from[1] + (to[1] - from[1]) * amount),
		Math.round(from[2] + (to[2] - from[2]) * amount),
	];
}

function dotsOf(art: Art): readonly Dot[] {
	const cached = DOT_CACHE.get(art);
	if (cached) return cached;
	const dots: Dot[] = [];
	for (const [y, row] of art.glyphs.entries()) {
		for (const [x, glyph] of Array.from(row).entries()) {
			const mask = glyph.codePointAt(0)! - 0x2800;
			if (mask <= 0) continue;
			const tone = TONES.indexOf(art.tones[y]![x]!);
			for (const [dy, pair] of DOT_BITS.entries()) {
				for (const [dx, bit] of pair.entries()) {
					if ((mask & bit) === 0) continue;
					const u = x * 2 + dx;
					const v = y * 4 + dy;
					dots.push({ x, y, bit, tone, lock: 0.12 + seed(u, v) * 0.62, hidden: seed(v * 3 + 1, u * 5 + 2) });
				}
			}
		}
	}
	DOT_CACHE.set(art, dots);
	return dots;
}

export function renderLogoIntro(styler: HeaderStyler, large: boolean, width: number, frame: number): string[] {
	const art = large ? HERO_120 : HERO_80;
	const palette = paletteFor(styler);
	const peak = peakColor(palette);
	const noise = noiseColor(palette);
	const progress = Math.min(1, frame / (INTRO_FRAMES - 1));
	const left = Math.max(0, Math.floor((width - art.width) / 2));
	const rows = art.glyphs.length;
	const masks = new Uint8Array(width * rows);
	const weights = new Float32Array(width * rows).fill(-1);
	const colors: Rgb[] = [];
	const place = (cell: number, bit: number, color: Rgb, weight: number) => {
		masks[cell] = masks[cell]! | bit;
		if (weight < weights[cell]!) return;
		weights[cell] = weight;
		colors[cell] = color;
	};
	for (const [index, dot] of dotsOf(art).entries()) {
		if (progress >= dot.lock) {
			const settle = Math.min(1, (progress - dot.lock) / 0.1);
			place(dot.y * width + left + dot.x, dot.bit, blend(peak, palette[dot.tone]!, settle), 2 - settle);
			continue;
		}
		if (dot.hidden > 0.55 - progress * 0.4) continue;
		const spread = 1 - progress;
		const u = Math.round((left + dot.x) * 2 + 1 + (seed(index, frame) + seed(frame + 11, index) - 1) * art.width * 1.1 * spread);
		const v = Math.round(dot.y * 4 + 2 + (seed(frame * 7 + 3, index) + seed(index, frame * 5 + 1) - 1) * rows * 3 * spread);
		if (u < 0 || u >= width * 2 || v < 0 || v >= rows * 4) continue;
		place((v >> 2) * width + (u >> 1), DOT_BITS[v & 3]![u & 1]!, noise, 0);
	}
	return Array.from({ length: rows }, (_, y) => {
		let output = "";
		let color = "";
		let gap = 0;
		for (let x = 0; x < width; x++) {
			const cell = y * width + x;
			const mask = masks[cell]!;
			if (mask === 0) {
				gap += 1;
				continue;
			}
			const [r, g, b] = colors[cell]!;
			const next = ink(r, g, b);
			output += " ".repeat(gap);
			gap = 0;
			if (next !== color) {
				output += next;
				color = next;
			}
			output += String.fromCharCode(0x2800 + mask);
		}
		return color ? `${output}\x1b[39m` : output;
	});
}

export function introArtRows(large: boolean): number {
	return (large ? HERO_120 : HERO_80).glyphs.length;
}

export function renderIntroVeil(styler: HeaderStyler, width: number, rows: number, frame: number): string[] {
	const progress = Math.min(1, frame / (INTRO_FRAMES - 1));
	const wipe = Math.max(0, (progress - 0.7) / 0.3);
	const height = Math.round(rows * (1 - wipe * wipe * (3 - 2 * wipe)));
	const [r, g, b] = noiseColor(paletteFor(styler));
	const dust = ink(r, g, b);
	return Array.from({ length: height }, (_, index) => {
		const y = rows - height + index;
		const density = 0.09 * (1 - progress) * (1 - y / rows) ** (1 + progress * 3);
		let output = "";
		let gap = 0;
		for (let x = 0; x < width; x++) {
			const roll = seed(x * 31 + y, frame * 17 + 5);
			if (roll >= density) {
				gap += 1;
				continue;
			}
			const dot = DOT_BITS[Math.floor(roll * 97) & 3]![Math.floor(roll * 389) & 1]!;
			output += `${" ".repeat(gap)}${dust}${String.fromCharCode(0x2800 + dot)}\x1b[39m`;
			gap = 0;
		}
		return output + " ".repeat(gap);
	});
}
