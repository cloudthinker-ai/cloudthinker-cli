import { InteractiveMode, keyText } from "@earendil-works/pi-coding-agent";
import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { Container, Text } from "@earendil-works/pi-tui";
import type { Component } from "@earendil-works/pi-tui";

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
	settingsManager: { getQuietStartup(): boolean };
	getStartupExpansionState(): boolean;
	showLoadedResources(options?: ResourceOptions): void;
	formatDiagnostics(diagnostics: { type: string }[], sources: unknown): string;
}

class StartupResources implements Component {
	private readonly details: Component[];
	private readonly summary: Text;
	private expanded = false;

	constructor(details: Component[], summary: string, expanded: boolean) {
		this.details = details;
		this.summary = new Text(summary, 1, 0);
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
		this.summary.invalidate();
		for (const detail of this.details) detail.invalidate();
	}

	render(width: number): string[] {
		return this.expanded
			? this.details.flatMap((detail) => detail.render(width))
			: this.summary.render(width);
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
		const view = Object.create(this) as StartupHost;
		view.formatDiagnostics = (diagnostics, sources) => {
			for (const diagnostic of diagnostics) {
				if (diagnostic.type === "error") errors += 1;
				else warnings += 1;
			}
			return this.formatDiagnostics(diagnostics, sources);
		};
		original.call(view, options);
		const container = this.loadedResourcesContainer;
		if (container.children.length === 0) return;
		const summary: string[] = [];
		if (options?.force || this.options.verbose || !this.settingsManager.getQuietStartup()) {
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
		const issues: string[] = [];
		if (errors) issues.push(theme.fg("error", `${errors} startup error${errors === 1 ? "" : "s"}`));
		if (warnings) issues.push(theme.fg("warning", `${warnings} startup warning${warnings === 1 ? "" : "s"}`));
		if (issues.length) summary.push(`${issues.join(" · ")} · ${theme.fg("muted", `${keyText("app.tools.expand")} details`)}`);
		const details = [...container.children];
		container.clear();
		container.addChild(new StartupResources(details, summary.join("\n"), this.getStartupExpansionState()));
	};
}
