import { hostname } from "node:os";

import { keyHint, keyText, rawKeyHint } from "@earendil-works/pi-coding-agent";
import type { TerminalInputHandler, Theme, ThemeColor } from "@earendil-works/pi-coding-agent";
import { isKeyRelease, parseKey, Text, visibleWidth } from "@earendil-works/pi-tui";
import type { OverlayHandle, TUI } from "@earendil-works/pi-tui";

import { sanitizeTerminalText } from "./awareness.ts";
import { INTRO_FRAMES, introArtRows, MARK_HEIGHT, MARK_WIDTH, renderIntroVeil, renderLogo, renderLogoIntro, renderMark } from "./logo.ts";
import { introDue, recordIntro } from "./settings.ts";
import { setStartupHeroVisible, startupDetailRows, startupHeroVisible, startupDetailsRevision, startupDiagnosticRows } from "./startup-presentation.ts";
import { PI_AUTHOR, PI_LICENSE, type HostVersions } from "./versions.ts";
import { reducedMotion } from "./motion.ts";

declare module "@earendil-works/pi-tui" {
	interface Keybindings {
		"app.screen.clear": true;
		"app.prompt.search": true;
	}
}

export const PRODUCT_NAME = "cloudthinker";
export const UNLINKED_LINE = "not linked · cloud tools unavailable · /cloud retry";
export const LINKING_LINE = "linking…";

export type LinkState = "linking" | "linked" | "unavailable";
export type HeaderPresentation = "launch" | "session";

export interface HeaderState {
	link: LinkState;
	workspaceName?: string;
	userEmail?: string;
	webUrl?: string;
	autoMode?: boolean;
	cloudEnabled?: boolean;
	host?: string;
}

export const REMOTE_DETACH_KEY = "ctrl+z";

export function remoteHost(env: Readonly<Record<string, string | undefined>> = process.env): string | undefined {
	const remote = env.CLOUDTHINKER_REMOTE === "1" || [env.SSH_CONNECTION, env.SSH_CLIENT, env.SSH_TTY, env.MOSH_CONNECTION].some(Boolean);
	return remote ? hostname() : undefined;
}

export type IntroCadence = { readonly stride: number; readonly intervalMs: number; readonly veil: boolean };

export function introCadence(env: Readonly<Record<string, string | undefined>> = process.env): IntroCadence {
	return reducedMotion(env) ? { stride: 4, intervalMs: 100, veil: false } : { stride: 1, intervalMs: 50, veil: true };
}

export const AUTO_LABEL = "Auto";
export const MANUAL_LABEL = "Manual";

function terminalEnvironment(): { monochrome: boolean; ascii: boolean; intro: boolean; motion: boolean } {
	const term = process.env.TERM ?? "";
	const dumb = term === "dumb";
	const consoleFont = term === "linux" || term.startsWith("vt");
	const nonInteractive = process.stdout.isTTY !== true;
	const live = process.env.NO_COLOR === undefined && !dumb && !nonInteractive;
	return {
		monochrome: process.env.NO_COLOR !== undefined || dumb || nonInteractive,
		ascii: dumb || consoleFont || nonInteractive,
		intro: live && !consoleFont && process.env.CLOUDTHINKER_CLI_MOTION !== "0",
		motion: live && process.env.CLOUDTHINKER_CLI_MOTION === "1",
	};
}

function isKeyPress(data: string): boolean {
	return !isKeyRelease(data) && (!data.startsWith("\x1b") || parseKey(data) !== undefined);
}

export interface HeaderStyler {
	fg(color: ThemeColor, text: string): string;
	bold(text: string): string;
	getFgAnsi?(color: ThemeColor): string;
}

export interface HeaderHints {
	compact: string;
	expanded: string;
}

function formatSessionLine(state: HeaderState, styler: HeaderStyler): string {
	if (state.link === "unavailable") {
		return styler.fg("error", state.cloudEnabled === false ? `${UNLINKED_LINE} · Cloud:Off` : UNLINKED_LINE);
	}
	if (state.link === "linking") {
		return state.cloudEnabled === false ? styler.fg("muted", "Cloud:Off") : styler.fg("dim", LINKING_LINE);
	}
	const parts: string[] = [];
	if (state.workspaceName) parts.push(styler.fg("text", `${sanitizeTerminalText(state.workspaceName)} (cloud)`));
	if (state.autoMode !== undefined) {
		parts.push(styler.fg("muted", state.autoMode ? AUTO_LABEL : MANUAL_LABEL));
	}
	if (state.cloudEnabled !== undefined) parts.push(styler.fg("muted", `Cloud:${state.cloudEnabled ? "On" : "Off"}`));
	if (state.host) parts.push(styler.fg("muted", `on ${sanitizeTerminalText(state.host)}`));
	return parts.join(styler.fg("muted", " · "));
}

function centerLine(value: string, width: number): string {
	const visible = visibleWidth(value);
	return `${" ".repeat(Math.max(0, Math.floor((width - visible) / 2)))}${value}`;
}

export function buildHints(styler: HeaderStyler, remote = process.env.CLOUDTHINKER_REMOTE === "1"): HeaderHints {
	const separator = styler.fg("muted", " · ");
	const leave = remote
		? [rawKeyHint(REMOTE_DETACH_KEY, "detach"), keyHint("app.exit", "quit")]
		: [rawKeyHint(`${keyText("app.clear")}/${keyText("app.exit")}`, "clear/exit")];
	return {
		compact: [
			keyHint("app.interrupt", "interrupt"),
			...leave,
			rawKeyHint("/", "commands"),
			rawKeyHint("!", "bash"),
			keyHint("app.tools.expand", "transcript"),
		].join(separator),
		expanded: [
			keyHint("app.interrupt", "to interrupt"),
			keyHint("app.clear", "to clear"),
			rawKeyHint(`${keyText("app.clear")} twice`, "to exit"),
			keyHint("app.exit", "to exit (empty)"),
			remote
				? rawKeyHint(REMOTE_DETACH_KEY, "to detach (the agent keeps running)")
				: keyHint("app.suspend", "to suspend"),
			keyHint("tui.editor.deleteToLineEnd", "to delete to end"),
			rawKeyHint(
				`${keyText("app.model.cycleForward")}/${keyText("app.model.cycleBackward")}`,
				"to cycle models",
			),
			rawKeyHint("/model", "to select model"),
			keyHint("app.screen.clear", "to clear the screen"),
			keyHint("app.prompt.search", "to search past prompts"),
			keyHint("app.tools.expand", "to open the transcript"),
			keyHint("app.thinking.toggle", "to show or hide thinking"),
			keyHint("app.editor.external", "for external editor"),
			rawKeyHint("/", "for commands"),
			rawKeyHint("!", "to run bash"),
			rawKeyHint("!!", "to run bash (no context)"),
			keyHint("app.message.followUp", "to queue follow-up"),
			keyHint("app.message.dequeue", "to edit all queued messages"),
			keyHint("app.clipboard.pasteImage", "to paste image (with text fallback)"),
			rawKeyHint("drop files", "to attach"),
		].join("\n"),
	};
}

export function formatHeaderText(
	state: HeaderState,
	versions: HostVersions,
	styler: HeaderStyler,
	hints: HeaderHints,
	expanded: boolean,
	width = 80,
	motionStep?: number,
	presentation: HeaderPresentation = "session",
): string {
	const title = `${styler.bold(styler.fg("accent", "CloudThinker"))}${styler.fg("muted", ` v${versions.host}`)}`;
	const identity = [title];
	if (state.link === "linked") {
		identity.push(formatSessionLine(state, styler));
		if (state.userEmail) identity.push(styler.fg("muted", sanitizeTerminalText(state.userEmail)));
		if (state.webUrl) identity.push(styler.fg("muted", "/open · view session in browser"));
	} else {
		identity.push(formatSessionLine(state, styler));
	}
	const lines: string[] = [];
	const environment = terminalEnvironment();
	const rowCount = process.stdout.rows ?? 24;
	const launch = presentation === "launch" && !expanded;
	const largeLogo = launch && width >= 112 && rowCount >= 40;
	const logo = launch ? renderLogo(styler, environment.monochrome, largeLogo, environment.ascii) : [];
	const status = [
		state.cloudEnabled === false
			? "Cloud:Off"
			: state.link === "linked"
			? (state.workspaceName ? undefined : "session linked")
			: state.link === "unavailable" ? "not linked" : "linking cloud",
		state.workspaceName ? sanitizeTerminalText(state.workspaceName) : undefined,
		state.autoMode === undefined ? undefined : state.autoMode ? AUTO_LABEL : MANUAL_LABEL,
		state.userEmail ? sanitizeTerminalText(state.userEmail) : undefined,
		state.cloudEnabled === true || (state.cloudEnabled === undefined && state.link === "linked") ? "Cloud:On" : undefined,
		state.webUrl ? "/open" : undefined,
		state.host ? `on ${sanitizeTerminalText(state.host)}` : undefined,
	].filter((part): part is string => part !== undefined).join(" · ");
	const heroFits = launch && width >= 64 && visibleWidth(status) <= width
		&& rowCount >= logo.length + 2 + 7 + startupDiagnosticRows();
	setStartupHeroVisible(heroFits);
	if (heroFits) {
		if (motionStep === undefined) for (const row of logo) lines.push(centerLine(row, width));
		else lines.push(...renderLogoIntro(styler, largeLogo, width, motionStep));
		lines.push(centerLine(
			`${styler.bold(styler.fg("text", "CloudThinker"))} ${styler.bold(styler.fg("accent", `v${versions.host}`))}`,
			width,
		));
		lines.push(centerLine(styler.fg(state.link === "unavailable" ? "error" : "muted", status), width));
		if (versions.pi && rowCount >= logo.length + 2 + 7 + startupDiagnosticRows() + 1) {
			lines.push(centerLine(styler.fg("dim", `built on pi v${versions.pi} by ${PI_AUTHOR} (${PI_LICENSE})`), width));
		}
	} else if (width >= 64) {
		const mark = renderMark(styler, environment.monochrome, environment.ascii, launch ? undefined : motionStep);
		const detailWidth = Math.max(1, width - MARK_WIDTH - 1);
		const compactIdentity = [
			title,
			formatSessionLine(state, styler),
			state.userEmail ? styler.fg("muted", sanitizeTerminalText(state.userEmail)) : "",
			state.webUrl ? styler.fg("muted", "/open · view session in browser") : "",
		].join("\n");
		const sessionIdentity = expanded
			? [...identity, ...(versions.pi ? [styler.fg("muted", `built on pi v${versions.pi} by ${PI_AUTHOR} (${PI_LICENSE})`)] : [])].join("\n")
			: compactIdentity;
		const details = new Text(sessionIdentity, 0, 0).render(detailWidth);
		for (let row = 0; row < Math.max(mark.length, details.length); row++) {
			lines.push(`${mark[row] ?? " ".repeat(MARK_WIDTH)} ${details[row] ?? ""}`);
		}
	} else {
		setStartupHeroVisible(false);
		lines.push(title, formatSessionLine(state, styler));
		if (state.userEmail) lines.push(styler.fg("muted", sanitizeTerminalText(state.userEmail)));
		if (state.webUrl) lines.push(styler.fg("muted", "/open · view session in browser"));
	}
	if (expanded && versions.pi && width < 64) {
		lines.push(styler.fg("muted", `built on pi v${versions.pi} by ${PI_AUTHOR} (${PI_LICENSE})`));
	}
	if (expanded && state.webUrl) lines.push(styler.fg("muted", sanitizeTerminalText(state.webUrl)));
	if (launch) return lines.join("\n");
	if (expanded) {
		const legend = styler.fg("muted", "[L] this machine · [C] sandbox · /skill:tour get started");
		const legendRows = new Text(legend, 0, 0).render(width).length;
		const expandedRows = new Text(hints.expanded, 0, 0).render(width).length;
		const availableRows = (process.stdout.rows ?? 24) - lines.length - legendRows - 7 - startupDetailRows(width);
		if (expandedRows <= availableRows) return [...lines, legend, hints.expanded].join("\n");
		return [...lines, hints.compact, legend].join("\n");
	}
	return lines.join("\n");
}

class HeaderComponent extends Text {
	private expanded = false;
	private motionStep: number | undefined;
	private readonly build: (expanded: boolean, width: number, motionStep?: number) => string;
	private readonly onDispose: () => void;
	private width = 80;
	private rows = process.stdout.rows ?? 24;
	private diagnosticRows = startupDiagnosticRows();
	private detailsRevision = startupDetailsRevision();

	constructor(
		build: (expanded: boolean, width: number, motionStep?: number) => string,
		onDispose: () => void,
	) {
		super(build(false, 78), 1, 0);
		this.build = build;
		this.onDispose = onDispose;
	}

	dispose(): void {
		setStartupHeroVisible(false);
		this.onDispose();
	}

	setExpanded(expanded: boolean): void {
		this.expanded = expanded;
		this.refresh();
	}

	setMotionStep(row: number | undefined): void {
		this.motionStep = row;
		this.refresh();
	}

	refresh(): void {
		this.setText(this.build(this.expanded, Math.max(1, this.width - 2), this.motionStep));
	}

	override render(width: number): string[] {
		const rows = process.stdout.rows ?? 24;
		const diagnosticRows = startupDiagnosticRows();
		const detailsRevision = startupDetailsRevision();
		if (width !== this.width || rows !== this.rows || diagnosticRows !== this.diagnosticRows || detailsRevision !== this.detailsRevision) {
			this.width = width;
			this.rows = rows;
			this.diagnosticRows = diagnosticRows;
			this.detailsRevision = detailsRevision;
			this.refresh();
		}
		return super.render(width);
	}
}

export class SessionHeader {
	private state: HeaderState = { link: "linking", host: remoteHost() };
	private presentation: HeaderPresentation = "launch";
	private component: HeaderComponent | undefined;
	private tui: TUI | undefined;
	private readonly versions: HostVersions;
	private animationTimer: NodeJS.Timeout | undefined;
	private stopped = false;

	constructor(versions: HostVersions) {
		this.versions = versions;
	}

	factory = (tui: TUI, theme: Theme): HeaderComponent => {
		const hints = buildHints(theme);
		const build = (expanded: boolean, width: number, motionStep?: number) =>
			formatHeaderText(this.state, this.versions, theme, hints, expanded, width, motionStep, this.presentation);
		if (this.stopped) {
			return new HeaderComponent(build, () => {});
		}
		this.cancelMotion();
		this.tui = tui;
		this.theme = theme;
		let component: HeaderComponent;
		this.heroShown = undefined;
		component = new HeaderComponent((expanded, width, motionStep) => {
			const text = build(expanded, width, motionStep);
			this.heroShown = startupHeroVisible();
			return text;
		}, () => {
			if (this.component !== component) return;
			this.cancelMotion();
			this.motionStep = undefined;
			this.component = undefined;
			this.tui = undefined;
		});
		this.component = component;
		this.startMotion();
		return component;
	};

	private motionStep: number | undefined;
	private heroShown: boolean | undefined;
	private theme: Theme | undefined;
	private veil: OverlayHandle | undefined;
	private introPlayed = false;
	private cadence = introCadence();
	private sweepPlayed = false;
	private listenInput: ((handler: TerminalInputHandler) => () => void) | undefined;
	private stopInput: (() => void) | undefined;

	listen(listenInput: (handler: TerminalInputHandler) => () => void): void {
		this.listenInput = listenInput;
	}

	private scheduleMotion(step: number, steps: number): void {
		this.animationTimer = setTimeout(() => {
			this.animationTimer = undefined;
			if (this.stopped || !this.component || !this.tui) return;
			if (step === 1 && this.presentation === "launch") {
				if (this.heroShown === undefined) {
					this.scheduleMotion(step, steps);
					return;
				}
				if (!this.heroShown) {
					this.finishMotion();
					return;
				}
				recordIntro(this.versions.host);
				if (this.cadence.veil) this.showVeil();
			}
			if (step >= steps) {
				this.finishMotion();
				return;
			}
			this.motionStep = step;
			this.component.setMotionStep(step);
			this.tui.requestRender();
			this.scheduleMotion(step >= steps - 1 ? steps : Math.min(steps - 1, step + this.cadence.stride), steps);
		}, this.cadence.intervalMs);
		this.animationTimer.unref();
	}

	private cancelMotion(): void {
		if (this.animationTimer) clearTimeout(this.animationTimer);
		this.animationTimer = undefined;
		this.stopInput?.();
		this.stopInput = undefined;
		this.veil?.hide();
		this.veil = undefined;
	}

	private showVeil(): void {
		const theme = this.theme;
		if (!this.tui || !theme) return;
		this.veil = this.tui.showOverlay({
			render: (width: number) => {
				const rows = process.stdout.rows ?? 24;
				const large = width - 2 >= 112 && rows >= 40;
				return renderIntroVeil(theme, width, Math.max(0, rows - 1 - introArtRows(large)), this.motionStep ?? INTRO_FRAMES);
			},
			invalidate: () => {},
		}, { anchor: "bottom-left", width: "100%", nonCapturing: true });
	}

	private finishMotion(): void {
		this.cancelMotion();
		this.motionStep = undefined;
		this.component?.setMotionStep(undefined);
		this.tui?.requestRender();
	}

	private motionSteps(): number | undefined {
		if (this.presentation === "launch") {
			if (this.introPlayed || !this.component || !this.tui) return undefined;
			this.introPlayed = true;
			const environment = terminalEnvironment();
			return environment.intro && introDue(this.versions.host, environment.motion) ? INTRO_FRAMES : undefined;
		}
		if (this.sweepPlayed || this.state.link !== "linked") return undefined;
		this.sweepPlayed = true;
		return this.component && this.tui && terminalEnvironment().motion ? MARK_HEIGHT : undefined;
	}

	private startMotion(): void {
		const steps = this.motionSteps();
		if (steps !== undefined) {
			this.cancelMotion();
			this.motionStep = 0;
			this.scheduleMotion(1, steps);
			this.stopInput = this.listenInput?.((data) => {
				if (isKeyPress(data)) this.finishMotion();
				return undefined;
			});
		}
		this.component?.setMotionStep(this.motionStep);
		this.tui?.requestRender();
	}

	reset(): void {
		this.cancelMotion();
		this.motionStep = undefined;
		if (this.stopped) return;
		this.presentation = "launch";
		this.set({
			link: "linking",
			workspaceName: undefined,
			userEmail: undefined,
			webUrl: undefined,
			autoMode: undefined,
		});
	}

	setPresentation(presentation: HeaderPresentation): void {
		if (this.stopped || this.presentation === presentation) return;
		setStartupHeroVisible(false);
		this.cancelMotion();
		this.motionStep = undefined;
		this.presentation = presentation;
		this.startMotion();
	}

	stop(): void {
		if (this.stopped) return;
		setStartupHeroVisible(false);
		this.stopped = true;
		this.cancelMotion();
		this.motionStep = undefined;
		this.component?.setMotionStep(undefined);
		this.component = undefined;
		this.tui = undefined;
	}

	set(next: Partial<HeaderState>): void {
		if (this.stopped) return;
		this.state = { ...this.state, ...next };
		this.startMotion();
	}
}
