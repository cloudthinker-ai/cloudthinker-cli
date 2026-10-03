import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { TuiAltScreen } from "@earendil-works/pi-tui";

import { keyDisplayText } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/keybinding-hints.js";
import { theme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";

export const JUMP_LABEL = "Jump to latest";

interface ScrollState {
	isFollowingEnd: boolean;
}

interface PillHost {
	implicitScrollView: ScrollState;
	scrollToEndIndicator: (() => string) | undefined;
}

type Composite = (this: PillHost, screen: string[], layout: { primaryScrollView?: ScrollState }, width: number) => string[];

interface PillPrototype {
	compositeScrollToEndIndicator: Composite;
}

let updates = 0;
const baselines = new WeakMap<object, number>();

export function noteTranscriptUpdate(): void {
	updates += 1;
}

export function transcriptUpdates(): number {
	return updates;
}

export function pillLabel(unseen: number, shortcut: string): string {
	const what = unseen > 0 ? `${unseen} new` : JUMP_LABEL;
	return ` ↓ ${what}${shortcut ? ` · ${shortcut}` : ""} `;
}

export function applyScrollPill(): void {
	const prototype = TuiAltScreen.prototype as unknown as PillPrototype;
	const composite = prototype.compositeScrollToEndIndicator;
	if (typeof composite !== "function") {
		throw new Error("pi-tui no longer exposes compositeScrollToEndIndicator, so the new-output pill cannot count");
	}
	prototype.compositeScrollToEndIndicator = function (screen, layout, width) {
		const scrollView = layout.primaryScrollView ?? this.implicitScrollView;
		const original = this.scrollToEndIndicator;
		if (!original || !scrollView || scrollView.isFollowingEnd) {
			baselines.delete(this);
			return composite.call(this, screen, layout, width);
		}
		if (!baselines.has(this)) baselines.set(this, updates);
		const unseen = updates - baselines.get(this)!;
		this.scrollToEndIndicator = () =>
			theme.bg("selectedBg", theme.fg("text", pillLabel(unseen, keyDisplayText("tui.altScreen.bottom"))));
		try {
			return composite.call(this, screen, layout, width);
		} finally {
			this.scrollToEndIndicator = original;
		}
	};
}

export function registerScrollPill(pi: ExtensionAPI): void {
	pi.on("message_end", (event) => {
		if (event.message.role !== "user") noteTranscriptUpdate();
	});
}
