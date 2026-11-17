import { InteractiveMode, keyText } from "@earendil-works/pi-coding-agent";
import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { Container, Text } from "@earendil-works/pi-tui";
import type { Component } from "@earendil-works/pi-tui";

import { machineBarLines } from "@cloudthinker/cloud/src/awareness.ts";
import { setStartupDiagnosticRows, setStartupDetailRowsReader, startupHeroVisible } from "@cloudthinker/cloud/src/startup-presentation.ts";
import { theme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";

interface ResourceOptions {
	force?: boolean;
	showDiagnosticsWhenQuiet?: boolean;
	extensions?: { path: string; sourceInfo?: unknown }[];
}

interface StartupHost {
	loadedResourcesContainer: Container;
	session: AgentSession;
	options: { verbose?: boolean };
	settingsManager: { getQuietStartup(): boolean | "header" };
	getStartupExpansionState(): boolean;
	showLoadedResources(options?: ResourceOptions): void;
	formatDiagnostics(diagnostics: { type: string }[], sources: unknown): string;
}

class StartupResources implements Component {
	private readonly details: Component[];
	private readonly lines: (compact: boolean) => string[];
	private expanded = false;

	constructor(details: Component[], lines: (compact: boolean) => string[], expanded: boolean) {
		this.details = details;
		this.lines = lines;
		this.setExpanded(expanded);
	}

	setExpanded(expanded: boolean): void {
		this.expanded = expanded;
		for (const detail of this.details) {
			if ("setExpanded" in detail && typeof detail.setExpanded === "function") {
				detail.setExpanded(expanded);
			}
		}
	}

	invalidate(): void {
		for (const detail of this.details) detail.invalidate();
	}

	render(width: number): string[] {
		if (this.expanded) return this.details.flatMap((detail) => detail.render(width));
		const compact = process.stdout.isTTY === true && process.stdout.rows <= 40;
		const text = this.lines(compact).join("\n");
		return text.length === 0 ? [] : new Text(text, 1, 0).render(width);
	}
}

export function applyStartupUi(): void {
	const prototype = InteractiveMode.prototype as unknown as StartupHost;
	const original = prototype.showLoadedResources;
	if (typeof original !== "function" || typeof prototype.formatDiagnostics !== "function") {
		throw new Error("pi's startup resource rendering seam changed");
	}
	prototype.showLoadedResources = function (options) {
		let warnings = 0;
		let errors = 0;
		let counting = true;
		const view = Object.create(this) as StartupHost;
		view.formatDiagnostics = (diagnostics, sources) => {
			for (const diagnostic of counting ? diagnostics : []) {
				if (diagnostic.type === "error") errors += 1;
				else warnings += 1;
			}
			return this.formatDiagnostics(diagnostics, sources);
		};
		original.call(view, options);
		const container = this.loadedResourcesContainer;
		for (const child of container.children) child.render(120);
		counting = false;
		const showMachineBar = Boolean(options?.force || this.options.verbose || !this.settingsManager.getQuietStartup());
		const summary: string[] = [];
		if (showMachineBar) {
			const loader = this.session.resourceLoader;
			const counts: [number, string][] = [
				[loader.getAgentsFiles().agentsFiles.length + loader.getAppendSystemPromptSources().length + Number(Boolean(loader.getSystemPromptSource())), "context file"],
				[loader.getSkills().skills.length, "skill"],
				[this.session.promptTemplates.length, "prompt"],
				[options?.extensions?.length ?? loader.getExtensions().extensions.filter((extension) => !extension.hidden).length, "extension"],
				[loader.getThemes().themes.filter((loadedTheme) => loadedTheme.sourcePath).length, "theme"],
			];
			summary.push(theme.fg("muted", counts.filter(([count]) => count > 0)
				.map(([count, name]) => `${count} ${name}${count === 1 ? "" : "s"}`).join(" · ")));
		}
		setStartupDiagnosticRows(0);
		setStartupDetailRowsReader(() => 0);
		if (container.children.length === 0 && !showMachineBar) return;
		const issues: string[] = [];
		if (errors) issues.push(theme.fg("error", `${errors} startup error${errors === 1 ? "" : "s"}`));
		if (warnings) issues.push(theme.fg("warning", `${warnings} startup warning${warnings === 1 ? "" : "s"}`));
		const issueLine = issues.length ? `${issues.join(" · ")} · ${theme.fg("muted", `${keyText("app.tools.expand")} details`)}` : undefined;
		setStartupDiagnosticRows(issueLine ? 1 : 0);
		if (issueLine) summary.push(issueLine);
		const details = [...container.children];
		setStartupDetailRowsReader((width) => details.reduce((rows, detail) => rows + detail.render(width).length, 0));
		container.clear();
		const lines = (compact: boolean) => startupHeroVisible()
			? issueLine ? [issueLine] : []
			: [
			...(showMachineBar
				? machineBarLines({ local: (text) => theme.fg("muted", text), cloud: (text) => theme.fg("accent", text) }, compact ? "compact" : "full")
				: []),
			...(compact ? issueLine ? [issueLine] : [] : summary),
		];
		container.addChild(new StartupResources(details, lines, this.getStartupExpansionState()));
	};
}
