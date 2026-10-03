import { copyToClipboard } from "@earendil-works/pi-coding-agent";
import { Markdown, Marked, type Token } from "@earendil-works/pi-tui";

import { AssistantMessageComponent } from "../node_modules/@earendil-works/pi-coding-agent/dist/modes/interactive/components/assistant-message.js";

type Block = { type: string; text?: string };

interface NumberedMarkdown {
	theme: { codeBlockBorder(text: string): string };
	ctCodeIndex?: number;
}

const parser = new Marked();
const offsets = new WeakMap<object, number>();

function numbered(lang: string | undefined): boolean {
	return lang?.split(/\s+/)[0]?.toLowerCase() !== "mermaid";
}

export function renderedText(markdown: string): string {
	return markdown.trim().replace(/\t/g, "   ");
}

export function codeBlocks(markdown: string): string[] {
	const blocks: string[] = [];
	const visit = (tokens: readonly Token[]) => {
		for (const token of tokens) {
			if (token.type === "code") {
				if (numbered((token as { lang?: string }).lang)) blocks.push((token as { text: string }).text);
				continue;
			}
			const nested = token as { tokens?: Token[]; items?: Token[] };
			if (nested.items) visit(nested.items);
			if (nested.tokens) visit(nested.tokens);
		}
	};
	visit(parser.lexer(renderedText(markdown)));
	return blocks;
}

export function answerCodeBlocks(content: readonly Block[]): string[] {
	return content.flatMap((block) => (block.type === "text" && block.text ? codeBlocks(block.text) : []));
}

export const COPY_USAGE = "Usage: /copy copies the last answer; /copy N copies its code block N.";

export function copyCommandIndex(text: string): number | "usage" | undefined {
	const match = /^\/copy\s+(\S.*)$/.exec(text.trim());
	if (!match) return undefined;
	return /^[1-9]\d*$/.test(match[1]!) ? Number(match[1]) : "usage";
}

export interface CopyHost {
	session: { messages: readonly { role: string; content?: readonly Block[] }[] };
	showStatus(message: string): void;
	showError(message: string): void;
}

export async function copyCodeBlock(host: CopyHost, index: number, copy: (text: string) => Promise<void> = copyToClipboard): Promise<void> {
	const answer = host.session.messages.findLast((message) => message.role === "assistant" && (message.content?.length ?? 0) > 0);
	const blocks = answerCodeBlocks(answer?.content ?? []);
	const block = blocks[index - 1];
	if (block === undefined) {
		host.showError(blocks.length === 0 ? "The last answer has no code blocks." : `The last answer has code blocks 1–${blocks.length}.`);
		return;
	}
	try {
		await copy(block);
		const lines = block.split("\n").length;
		host.showStatus(`Copied code block ${index} (${lines} ${lines === 1 ? "line" : "lines"})`);
	} catch (error) {
		host.showError(error instanceof Error ? error.message : String(error));
	}
}

export function applyCodeBlockNumbers(): void {
	const markdown = Markdown.prototype as unknown as {
		render(this: NumberedMarkdown, width: number): string[];
		renderToken(this: NumberedMarkdown, token: Token, ...rest: unknown[]): string[];
	};
	const { render, renderToken } = markdown;
	if (typeof renderToken !== "function") {
		throw new Error("pi-tui's Markdown no longer renders through renderToken, so code blocks cannot be numbered");
	}
	markdown.render = function (width) {
		const offset = offsets.get(this);
		if (offset !== undefined) this.ctCodeIndex = offset;
		return render.call(this, width);
	};
	markdown.renderToken = function (token, ...rest) {
		const lines = renderToken.call(this, token, ...rest);
		const lang = (token as { lang?: string }).lang;
		if (token.type !== "code" || this.ctCodeIndex === undefined || !numbered(lang)) return lines;
		this.ctCodeIndex += 1;
		const label = this.theme.codeBlockBorder(lang ? `[${this.ctCodeIndex}] ${lang}` : `[${this.ctCodeIndex}]`);
		if (lang) lines[0] = label;
		else lines.unshift(label);
		return lines;
	};
	const assistant = AssistantMessageComponent.prototype as unknown as {
		updateContent(this: { contentContainer: { children: object[] } }, message: { content: readonly Block[] }, isStreaming?: boolean): void;
	};
	const updateContent = assistant.updateContent;
	assistant.updateContent = function (message, isStreaming) {
		updateContent.call(this, message, isStreaming);
		const texts = message.content.filter((block) => block.type === "text" && block.text?.trim());
		const children = this.contentContainer.children.filter((child) => child instanceof Markdown);
		if (children.length !== texts.length) return;
		let offset = 0;
		children.forEach((child, index) => {
			offsets.set(child, offset);
			offset += codeBlocks(texts[index]!.text!).length;
		});
	};
}
