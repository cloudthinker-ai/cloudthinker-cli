import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { stripVTControlCharacters } from "node:util";

import { InteractiveMode, SettingsManager, createEditToolDefinition, createGrepToolDefinition, createReadToolDefinition, createWriteToolDefinition, type SessionEntry, type ToolDefinition } from "@earendil-works/pi-coding-agent";
import { CombinedAutocompleteProvider, Container, Editor, Text, TuiAltScreen, type TUI } from "@earendil-works/pi-tui";

import { KeybindingsManager } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/keybindings.js";
import { AssistantMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/assistant-message.js";
import { ToolExecutionComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/tool-execution.js";
import { getEditorTheme, initTheme, theme } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/theme/theme.js";
import { setConnectionSource } from "@cloudthinker/cloud/src/connection-mentions.ts";
import { tagLocalToolDefinition } from "../src/awareness.ts";
import { applyCodeBlockNumbers, copyCodeBlock, copyCommandIndex } from "../src/code-blocks.ts";
import { applyFilePicker } from "../src/file-picker.ts";
import { applyMentionPicker, createMentionPicker, stepFilter } from "../src/mention-picker.ts";
import { applyMentionHighlight } from "../src/mention-highlight.ts";
import { applyToolGroups, assignGroups } from "../src/tool-groups.ts";
import { PromptSearch, pastPrompts } from "../src/prompt-search.ts";
import { CLEAR_SCREEN, CLEAR_SCREEN_BUSY, MODEL_SELECT, applyClearScreenKey, clearScreen } from "../src/clear-screen.ts";
import { applyQueuedMessagesUi } from "../src/queue-ui.ts";
import { applyScrollPill, noteTranscriptUpdate } from "../src/scroll-pill.ts";
import { TranscriptView, applyTranscriptUi, transcriptMarkdown } from "../src/transcript.ts";
import { savedTuiMode, tuiModeArgs } from "../src/tui-mode.ts";

initTheme("dark");
applyTranscriptUi();
applyQueuedMessagesUi();
applyScrollPill();
applyClearScreenKey();
applyCodeBlockNumbers();
applyFilePicker();
applyMentionPicker();
applyMentionHighlight();
applyToolGroups();

const prototype = InteractiveMode.prototype as unknown as {
	toggleToolOutputExpansion(this: unknown): void;
	updatePendingMessagesDisplay(this: unknown): void;
};

function message(id: string, body: Record<string, unknown>): SessionEntry {
	return { type: "message", id, parentId: null, timestamp: "2026-10-01T00:00:00.000Z", message: { timestamp: 0, ...body } } as unknown as SessionEntry;
}

const conversation: SessionEntry[] = [
	message("a", { role: "user", content: "Why is the api pod restarting?" }),
	message("b", {
		role: "assistant",
		content: [
			{ type: "thinking", thinking: "PRIVATE_REASONING" },
			{ type: "text", text: "Checking the pod events." },
			{ type: "toolCall", id: "t1", name: "bash", arguments: { command: "kubectl describe pod api" } },
		],
		stopReason: "toolUse",
	}),
	message("c", {
		role: "toolResult",
		toolCallId: "t1",
		toolName: "bash",
		isError: false,
		content: [{ type: "text", text: Array.from({ length: 60 }, (_, index) => `event ${index}: OOMKilled`).join("\n") }],
	}),
	message("d", { role: "assistant", content: [{ type: "text", text: "The container hits its memory limit." }], stopReason: "stop" }),
];

test("a fresh launch defaults to fullscreen, while a saved choice or an explicit flag wins", () => {
	assert.deepEqual(tuiModeArgs([], savedTuiMode(SettingsManager.inMemory())), ["--tui-mode", "fullscreen"]);
	assert.deepEqual(tuiModeArgs([], savedTuiMode(SettingsManager.inMemory({ tuiMode: "regular" }))), []);
	assert.deepEqual(tuiModeArgs(["--tui-mode", "regular"], undefined), []);
});

test("Ctrl+O opens a searchable full transcript instead of expanding tool output in place", async () => {
	const expanded: boolean[] = [];
	let shown: { factory: Function; options: { overlay: boolean } } | undefined;
	const host = {
		sessionManager: { getBranch: () => conversation },
		settingsManager: { getExternalEditorCommand: () => "true" },
		toolOutputExpanded: false,
		setToolsExpanded: (value: boolean) => expanded.push(value),
		showExtensionCustom: (factory: Function, options: { overlay: boolean }) => {
			shown = { factory, options };
			return new Promise(() => {});
		},
	};
	prototype.toggleToolOutputExpansion.call(host);
	await Promise.resolve();
	assert.equal(expanded.length, 0);
	assert.equal(shown?.options.overlay, true);

	let closed = false;
	const tui = { terminal: { rows: 12, columns: 80 }, requestRender: () => {}, stop: () => {}, start: () => {} } as unknown as TUI;
	const view = shown!.factory(tui, theme, {}, () => { closed = true; }) as TranscriptView;
	const screen = (): string => stripVTControlCharacters(view.render(80).join("\n"));
	assert.match(screen(), /The container hits its memory limit\./);
	view.handleInput("g");
	assert.match(screen(), /Why is the api pod restarting\?[\s\S]*∴ Thinking\n\s+PRIVATE_REASONING\n\n ● CloudThinker\n\s+Checking the pod events\./);
	for (const key of ["/", "e", "v", "e", "n", "t", " ", "5", "9", "\r"]) view.handleInput(key);
	assert.match(screen(), /event 59: OOMKilled/);
	assert.match(screen(), /"event 59" 1\/1/);
	view.handleInput("q");
	assert.equal(closed, true);

	const edited = transcriptMarkdown([message("e", { role: "toolResult", toolCallId: "t2", toolName: "edit", isError: false, content: [{ type: "text", text: "Edited app.ts" }], details: { diff: "-2 const b = 2;\n+2 const b = 20;" } })]);
	assert.match(edited, /Edited app\.ts\n\n-2 const b = 2;\n\+2 const b = 20;/);
	const markdown = transcriptMarkdown(conversation);
	assert.match(markdown, /## bash\n\n```\nkubectl describe pod api\n```/);
	assert.match(markdown, /event 0: OOMKilled[\s\S]*event 59: OOMKilled/);

	const empty = {
		sessionManager: { getBranch: () => [] },
		toolOutputExpanded: false,
		setToolsExpanded: (value: boolean) => expanded.push(value),
	};
	prototype.toggleToolOutputExpansion.call(empty);
	assert.deepEqual(expanded, [true]);
});

test("queued messages sit above the editor in gray with when each one sends", () => {
	const container = new Container();
	prototype.updatePendingMessagesDisplay.call({
		pendingMessagesContainer: container,
		getAllQueuedMessages: () => ({ steering: ["also check the node"], followUp: ["then open a PR\nwith the fix"] }),
		getAppKeyDisplay: () => "Alt+Up",
	});
	const rendered = stripVTControlCharacters(container.render(80).join("\n"));
	assert.match(rendered, /Queued 2 · Alt\+Up to edit/);
	assert.match(rendered, /› also check the node · after this step/);
	assert.match(rendered, /› then open a PR \(\+1 lines\) · after this turn/);
	container.clear();
	prototype.updatePendingMessagesDisplay.call({
		pendingMessagesContainer: container,
		getAllQueuedMessages: () => ({ steering: [], followUp: [] }),
		getAppKeyDisplay: () => "Alt+Up",
	});
	assert.deepEqual(container.render(80), []);
});

test("scrolling up in fullscreen shows how much new output arrived below", () => {
	const writes: string[] = [];
	const terminal = new Proxy({ columns: 40, rows: 6, write: (data: string) => writes.push(data) } as Record<string, unknown>, {
		get: (target, key) => (key in target ? target[key as string] : () => {}),
	});
	const tui = new TuiAltScreen(terminal as never, false, undefined, { scrollToEndIndicator: () => " ↓ Jump to latest message " });
	const frame = (): string => {
		writes.length = 0;
		(tui as unknown as { doRender(): void }).doRender();
		return stripVTControlCharacters(writes.join(""));
	};
	tui.start();
	try {
		for (let index = 0; index < 30; index += 1) tui.addChild(new Text(`line ${index}`, 0, 0));
		frame();
		const scrollView = (tui as unknown as { implicitScrollView: { scrollTo(top: number): void; scrollToEnd(): void } }).implicitScrollView;
		scrollView.scrollTo(0);
		assert.match(frame(), /↓ Jump to latest/);
		noteTranscriptUpdate();
		noteTranscriptUpdate();
		tui.addChild(new Text("line 30", 0, 0));
		assert.match(frame(), /↓ 2 new/);
		scrollView.scrollToEnd();
		assert.doesNotMatch(frame(), /↓/);
		scrollView.scrollTo(0);
		assert.match(frame(), /↓ Jump to latest/);
	} finally {
		tui.stop();
	}
});

test("Ctrl+L clears an idle screen and refuses while a task runs", () => {
	const keybindings = new KeybindingsManager();
	assert.equal(keybindings.matches("\x0c", CLEAR_SCREEN), true);
	assert.equal(keybindings.matches("\x0c", MODEL_SELECT), false);

	const chatContainer = new Container();
	chatContainer.addChild(new Text("old answer", 0, 0));
	const statuses: string[] = [];
	const renders: boolean[] = [];
	const session = { isStreaming: true, isBashRunning: false };
	const host = { chatContainer, session, showStatus: (message: string) => statuses.push(message), ui: { requestRender: (force = false) => renders.push(force) } };
	clearScreen(host);
	session.isStreaming = false;
	session.isBashRunning = true;
	clearScreen(host);
	assert.equal(chatContainer.children.length, 1);
	assert.deepEqual(statuses, [CLEAR_SCREEN_BUSY, CLEAR_SCREEN_BUSY]);
	session.isBashRunning = false;
	clearScreen(host);
	assert.equal(chatContainer.children.length, 0);
	assert.deepEqual(renders, [true]);
});

test("the transcript copies the block under the cursor or everything", async () => {
	const copied: string[] = [];
	const tui = { terminal: { rows: 12, columns: 80 }, requestRender: () => {} } as unknown as TUI;
	const view = new TranscriptView({
		entries: () => conversation,
		theme,
		tui,
		onClose: () => {},
		onOpenEditor: () => {},
		onCopy: async (text) => { copied.push(text); },
	});
	const screen = (): string => stripVTControlCharacters(view.render(80).join("\n"));
	for (const key of ["/", "e", "v", "e", "n", "t", " ", "3", "\r"]) view.handleInput(key);
	assert.match(screen(), /y copy bash result/);
	view.handleInput("y");
	await new Promise((resolve) => setImmediate(resolve));
	assert.equal(copied[0]?.split("\n").length, 60);
	assert.match(screen(), /Copied bash result \(60 lines\)/);
	view.handleInput("Y");
	await new Promise((resolve) => setImmediate(resolve));
	assert.match(copied[1] ?? "", /^# Transcript[\s\S]*## You[\s\S]*## bash result/);
});

test("answer code blocks are numbered and /copy N copies one", async () => {
	const content = [
		{ type: "text", text: "Run this:\n\n```bash\nkubectl get pods\n```" },
		{ type: "toolCall", id: "t1", name: "bash", arguments: {} },
		{ type: "text", text: "Then:\n\n```\nplain\n```\n\n```mermaid\nflowchart LR\n  A --> B\n```\n\n- item\n\n  ```ts\n  const x = 1;\n  ```" },
	];
	const component = new AssistantMessageComponent({ role: "assistant", content, stopReason: "stop" } as never, true);
	const rendered = stripVTControlCharacters(component.render(80).map((line) => line.trimEnd()).join("\n"));
	assert.match(rendered, /\[1\] bash\n\s*▎ kubectl get pods/);
	assert.match(rendered, /\[2\]\n\s*▎ plain/);
	assert.match(rendered, /\[3\] ts\n\s*▎ const x = 1;/);
	assert.doesNotMatch(rendered, /\[\d\] mermaid/);

	const copied: string[] = [];
	const notes: string[] = [];
	const host = {
		session: { messages: [{ role: "user", content: [] }, { role: "assistant", content }] },
		showStatus: (message: string) => notes.push(message),
		showError: (message: string) => notes.push(`error: ${message}`),
	};
	await copyCodeBlock(host, 3, async (text) => { copied.push(text); });
	await copyCodeBlock(host, 4, async (text) => { copied.push(text); });
	assert.deepEqual(copied, ["const x = 1;"]);
	assert.deepEqual(notes, ["Copied code block 3 (1 line)", "error: The last answer has code blocks 1–3."]);
	const tabbed = [{ type: "text", text: "Run:\n\n\tmake build\n\n```sh\necho hi\n```" }];
	const shown = stripVTControlCharacters(new AssistantMessageComponent({ role: "assistant", content: tabbed, stopReason: "stop" } as never, true).render(80).join("\n"));
	assert.match(shown, /\[1\] sh/);
	await copyCodeBlock({ ...host, session: { messages: [{ role: "assistant", content: tabbed }] } }, 1, async (text) => { copied.push(text); });
	assert.equal(copied.at(-1), "echo hi");
	assert.deepEqual(["/copy", "/copy 2", " /copy 12 ", "/copy two", "/copy 0", "/copy 1 2"].map(copyCommandIndex), [undefined, 2, 12, "usage", "usage", "usage"]);
});

test("@ ranks git files fuzzily with changed and recent files first", async () => {
	const repo = mkdtempSync(join(tmpdir(), "ct-picker-"));
	const git = (...args: string[]) => execFileSync("git", ["-C", repo, "-c", "user.name=test", "-c", "user.email=test@example.com", ...args]);
	try {
		mkdirSync(join(repo, "src"));
		for (const file of ["src/status-line.ts", "src/session.ts", "README.md", "ignored.log"]) writeFileSync(join(repo, file), "x\n");
		writeFileSync(join(repo, ".gitignore"), "*.log\n");
		git("init", "-q");
		git("add", ".");
		git("commit", "-qm", "initial");
		writeFileSync(join(repo, "src/session.ts"), "changed\n");
		writeFileSync(join(repo, "notes.md"), "new\n");
		rmSync(join(repo, "README.md"));
		const provider = new CombinedAutocompleteProvider([], repo, null);
		const suggest = async (text: string) =>
			(await provider.getSuggestions([text], 0, text.length, { signal: new AbortController().signal }))?.items.map((item) => item.value) ?? [];
		assert.equal((await suggest("@stln"))[0], "@src/status-line.ts");
		assert.deepEqual((await suggest("@")).slice(0, 2), ["@src/session.ts", "@notes.md"]);
		assert.ok(!(await suggest("@ignored")).includes("@ignored.log"));
		assert.ok(!(await suggest("@readme")).includes("@README.md"));
	} finally {
		rmSync(repo, { recursive: true, force: true });
	}
});

test("one @ lists files, Connections, and agents, and ←/→ filters by kind", async () => {
	const repo = mkdtempSync(join(tmpdir(), "ct-mentions-"));
	setConnectionSource(() => [
		{ prefix: "aws", alias: "prod", execution_method: "cli", description: "Production account" },
		{ prefix: "aws", alias: "stage", execution_method: "cli", description: "" },
		{ prefix: "gcp", alias: "main", execution_method: "cli", description: "" },
	]);
	try {
		execFileSync("git", ["-C", repo, "init", "-q"]);
		writeFileSync(join(repo, "prod-notes.md"), "x\n");
		const files = new CombinedAutocompleteProvider([], repo, null);
		const withAgent = {
			...files,
			getSuggestions: async (...args: Parameters<typeof files.getSuggestions>) => {
				const theirs = await files.getSuggestions(...args);
				return { prefix: theirs?.prefix ?? "@", items: [{ value: "@explore", label: "@explore", description: "start agent", kind: "agent" }, ...(theirs?.items ?? [])] };
			},
			applyCompletion: files.applyCompletion.bind(files),
		};
		const renders: number[] = [];
		const tui = { requestRender: () => renders.push(1), terminal: { rows: 40, columns: 100 } } as unknown as TUI;
		const editor = new Editor(tui, getEditorTheme());
		editor.setAutocompleteProvider(createMentionPicker(withAgent));
		const screen = () => editor.render(100).map((line) => stripVTControlCharacters(line).trimEnd());
		const shows = async (check: (lines: string[]) => boolean) => {
			for (let attempt = 0; attempt < 150 && !check(screen()); attempt++) await new Promise((resolve) => setTimeout(resolve, 20));
			return screen();
		};
		for (const character of "@") editor.handleInput(character);
		const opened = await shows((lines) => lines.some((line) => line.includes("prod-notes.md")));
		assert.ok(opened.some((line) => line.includes(" All ") && line.includes(" Connections ") && line.includes(" Agents")), opened.join("\n"));
		assert.ok(opened.some((line) => line.includes("@explore") && line.includes("Agent · start agent")));
		assert.ok(opened.some((line) => line.includes("#aws/prod") && line.includes("Connection · prod · Production account")));
		assert.ok(opened.some((line) => line.includes("prod-notes.md") && line.includes("File")));
		assert.ok(stepFilter("\x1b[C") && stepFilter("\x1b[C"));
		const connections = await shows((lines) => !lines.some((line) => line.includes("@explore")));
		assert.ok(connections.some((line) => line.includes("#aws/stage")) && !connections.some((line) => line.includes("@explore") || line.includes("prod-notes.md")), connections.join("\n"));
		for (const character of "prod") editor.handleInput(character);
		await shows((lines) => !lines.some((line) => line.includes("stage")));
		editor.handleInput("\t");
		assert.equal(editor.getText(), "#connection/aws/prod ");
		for (const character of " @") editor.handleInput(character);
		assert.ok((await shows((lines) => lines.some((line) => line.includes("@explore")))).some((line) => line.includes("@explore")), "a new @ token starts on All again");
		assert.ok(stepFilter("\x1b[D"));
		assert.ok((await shows((lines) => !lines.some((line) => line.includes("prod-notes.md")))).some((line) => line.includes("@explore")), "← from All wraps to Agents");
		assert.ok(stepFilter("\x1b[D"));
		const gcp = await shows((lines) => lines.some((line) => line.includes("gcp")));
		assert.ok(gcp.some((line) => line.includes("#gcp") && line.includes("main")) && !gcp.some((line) => line.includes("@explore")));
		editor.setText("");
		for (const character of "check #pro") editor.handleInput(character);
		const hashed = await shows((lines) => lines.some((line) => line.includes("#aws/prod")));
		assert.ok(!hashed.some((line) => line.includes(" All ")) && !hashed.some((line) => line.includes("#aws/stage")), hashed.join("\n"));
		editor.handleInput("\t");
		assert.equal(editor.getText(), "check #connection/aws/prod ");
		for (const character of "@prod-n") editor.handleInput(character);
		await shows((lines) => lines.some((line) => line.includes("prod-notes.md")));
		assert.ok(stepFilter("\x1b[C"));
		await shows((lines) => !lines.some((line) => line.includes("@explore")));
		editor.handleInput("\t");
		assert.equal(editor.getText(), "check #connection/aws/prod @./prod-notes.md ");
		const accent = theme.fg("accent", "\u0000").split("\u0000")[0]!;
		const error = theme.fg("error", "\u0000").split("\u0000")[0]!;
		assert.ok(accent && error && accent !== error);
		for (const character of "#connection/azure @explore") editor.handleInput(character);
		const painted = editor.render(100)[1]!;
		assert.ok(painted.includes(`${accent}#connection/aws/prod`) && painted.includes(`${accent}@./prod-notes.md`) && painted.includes(`${accent}@explore`), JSON.stringify(painted));
		assert.ok(painted.includes(`${error}#connection/azure`), "a Connection the workspace lacks shows red");
		assert.equal(stripVTControlCharacters(painted).trim(), "check #connection/aws/prod @./prod-notes.md #connection/azure @explore");
	} finally {
		setConnectionSource(() => []);
		rmSync(repo, { recursive: true, force: true });
	}
});

test("Ctrl+U closes a stale @ list, and a root file never reads as an agent", async () => {
	const repo = mkdtempSync(join(tmpdir(), "ct-mention-edit-"));
	try {
		execFileSync("git", ["-C", repo, "init", "-q"]);
		mkdirSync(join(repo, "src"));
		writeFileSync(join(repo, "src", "status-line.ts"), "x\n");
		writeFileSync(join(repo, "general-purpose"), "x\n");
		const files = new CombinedAutocompleteProvider([], repo, null);
		const withAgent = {
			...files,
			getSuggestions: async (...args: Parameters<typeof files.getSuggestions>) => {
				const theirs = await files.getSuggestions(...args);
				const agent = /(^|\s)@general/.test(args[0][0] ?? "") ? [{ value: "@general-purpose", label: "@general-purpose", description: "start agent", kind: "agent" }] : [];
				return theirs || agent.length ? { prefix: theirs?.prefix ?? "@general", items: [...agent, ...(theirs?.items ?? [])] } : null;
			},
			applyCompletion: files.applyCompletion.bind(files),
		};
		const tui = { requestRender: () => {}, terminal: { rows: 40, columns: 100 } } as unknown as TUI;
		const editor = new Editor(tui, getEditorTheme());
		editor.setAutocompleteProvider(createMentionPicker(withAgent));
		const screen = () => editor.render(100).map((line) => stripVTControlCharacters(line).trimEnd());
		const shows = async (check: (lines: string[]) => boolean) => {
			for (let attempt = 0; attempt < 150 && !check(screen()); attempt++) await new Promise((resolve) => setTimeout(resolve, 20));
			return screen();
		};
		const listed = (lines: string[]) => lines.some((line) => line.includes("←/→ filter"));
		for (const character of "check @status") editor.handleInput(character);
		assert.ok(listed(await shows(listed)), "the @status list opens");
		editor.handleInput("\x15");
		assert.equal(editor.getText(), "");
		assert.ok(!listed(await shows((lines) => !listed(lines))), "Ctrl+U closes the list with the token it completed");

		for (const character of "@general") editor.handleInput(character);
		const both = await shows((lines) => lines.some((line) => line.includes("File")) && lines.some((line) => line.includes("Agent")));
		assert.ok(both.some((line) => line.includes("@general-purpose") && line.includes("Agent")), both.join("\n"));
		assert.ok(stepFilter("\x1b[C"));
		await shows((lines) => !lines.some((line) => line.includes("Agent ·")));
		editor.handleInput("\t");
		assert.equal(editor.getText(), "@./general-purpose ");
	} finally {
		rmSync(repo, { recursive: true, force: true });
	}
});

test("Ctrl+R searches past prompts across this project's sessions, newest first", async () => {
	const dir = mkdtempSync(join(tmpdir(), "ct-prompts-"));
	try {
		const line = (role: string, text: string) => JSON.stringify({ type: "message", message: { role, content: [{ type: "text", text }] } });
		writeFileSync(join(dir, "older.jsonl"), [line("user", "check the api pods"), line("assistant", "done"), line("user", "open a PR\nwith the fix")].join("\n"));
		const prompts = await pastPrompts(dir, ["deploy the api"]);
		assert.deepEqual(prompts, ["deploy the api", "open a PR\nwith the fix", "check the api pods"]);
		let chosen: string | undefined = "unset";
		const search = new PromptSearch(prompts, theme, (prompt) => { chosen = prompt; });
		for (const key of ["a", "p", "i"]) search.handleInput(key);
		const screen = stripVTControlCharacters(search.render(80).join("\n"));
		assert.match(screen, /check the api pods\n› deploy the api\n search prompts: api▏\s+2 of 3/);
		search.handleInput("\x1b[A");
		search.handleInput("\r");
		assert.equal(chosen, "check the api pods");
		const drafted = new PromptSearch(prompts, theme, () => {}, "fix");
		assert.match(stripVTControlCharacters(drafted.render(80).join("\n")), /› open a PR ⏎ with the fix\n search prompts: fix▏\s+1 of 3/);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});

test("edit and write collapse to +added −removed and open to the full diff on click", async () => {
	const dir = mkdtempSync(join(tmpdir(), "ct-edit-"));
	try {
		writeFileSync(join(dir, "app.ts"), "const a = 1;\nconst b = 2;\nconst c = 3;\n");
		const ui = { requestRender: () => {} };
		const edit = new ToolExecutionComponent(
			"edit",
			"e1",
			{ path: "app.ts", edits: [{ oldText: "const b = 2;\n", newText: "const b = 20;\nconst d = 4;\n" }] },
			{},
			tagLocalToolDefinition("edit", createEditToolDefinition(dir) as ToolDefinition),
			ui as never,
			dir,
		);
		edit.setArgsComplete();
		await new Promise((resolve) => setTimeout(resolve, 50));
		edit.updateResult({ content: [{ type: "text", text: "Edited app.ts" }], isError: false } as never, false);
		const screen = (component: { render(width: number): string[] }) =>
			component.render(80).map((line) => stripVTControlCharacters(line).trimEnd()).join("\n");
		assert.match(screen(edit), /\[L\] edit app\.ts  \+2 −1/);
		assert.doesNotMatch(screen(edit), /const d = 4/);
		edit.setExpanded(true);
		assert.match(screen(edit), /-\d+ const b = 2;[\s\S]*\+\d+ const d = 4;/);

		const write = new ToolExecutionComponent(
			"write",
			"w1",
			{ path: "new.ts", content: "one\ntwo\nthree\n" },
			{},
			tagLocalToolDefinition("write", createWriteToolDefinition(dir) as ToolDefinition),
			ui as never,
			dir,
		);
		write.setArgsComplete();
		write.updateResult({ content: [{ type: "text", text: "Wrote new.ts" }], isError: false } as never, false);
		assert.match(screen(write), /\[L\] write new\.ts  \+3/);
		assert.doesNotMatch(screen(write), /two/);
		write.setExpanded(true);
		assert.match(screen(write), /one\n\s*two\n\s*three/);
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});

test("a run of read and search calls folds into one line that opens on click", () => {
	const ui = { requestRender: () => {} };
	const tool = (name: string, id: string, args: object, isError = false) => {
		const definition = name === "read" ? createReadToolDefinition("/repo") : createGrepToolDefinition("/repo");
		const component = new ToolExecutionComponent(name, id, args, {}, tagLocalToolDefinition(name, definition as ToolDefinition), ui as never, "/repo");
		component.setArgsComplete();
		component.updateResult({ content: [{ type: "text", text: isError ? "boom" : "line one\nline two" }], isError } as never, false);
		return component;
	};
	const thought = (text: string) =>
		new AssistantMessageComponent({ role: "assistant", content: [{ type: "thinking", thinking: text }, { type: "toolCall", id: "x", name: "read", arguments: {} }], stopReason: "toolUse" } as never, true);
	const chat = new Container();
	for (const child of [
		tool("read", "r1", { path: "src/a.ts" }),
		thought("check b next"),
		tool("read", "r2", { path: "src/b.ts" }),
		tool("read", "r3", { path: "src/a.ts", offset: 40 }),
		tool("grep", "g1", { pattern: "TODO" }),
		new Text("Here is what I found.", 0, 0),
		tool("read", "r4", { path: "src/c.ts" }),
		tool("read", "r5", { path: "src/missing.ts" }, true),
		tool("read", "r6", { path: "src/d.ts" }),
		thought("trailing thought"),
	]) chat.addChild(child);
	const screen = () => {
		assignGroups(chat.children, 80);
		return chat.render(80).map((line) => stripVTControlCharacters(line).trimEnd()).join("\n");
	};
	const folded = screen();
	assert.match(folded, /\[L\] ▸ Read 2 files, searched 1 pattern/);
	assert.doesNotMatch(folded, /src\/b\.ts/);
	assert.equal(folded.match(/Thinking\.\.\./g)?.length, 1);
	assert.match(folded, /src\/c\.ts[\s\S]*src\/missing\.ts[\s\S]*src\/d\.ts/);
	const header = folded.split("\n").findIndex((line) => line.includes("▸ Read 2 files"));
	chat.handleMouse({ type: "click", button: "left", x: 2, y: header, width: 80, height: folded.split("\n").length } as never);
	const opened = screen();
	(chat.children[1] as unknown as { thinkingVisibilityOverrides: Map<number, boolean> }).thinkingVisibilityOverrides.set(0, false);
	assert.match(screen(), /▾ Read 2 files, searched 1 pattern/);
	assert.match(opened, /▾ Read 2 files, searched 1 pattern[\s\S]*src\/a\.ts[\s\S]*Thinking[\s\S]*src\/b\.ts[\s\S]*TODO/);
});
