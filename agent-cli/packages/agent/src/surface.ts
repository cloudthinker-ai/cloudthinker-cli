import { PROVIDER_ID } from "@cloudthinker/cloud/src/provider.ts";

export const AGENT_HELP = `cloudthinker agent - CloudThinker Agent in your terminal

Usage:
  cloudthinker agent [options] [@files...] [prompt...]

Options:
  -p, --print <prompt>      Run headless: answer, print, and exit
  --mode json               With -p, stream events as JSON lines
  -c, --continue            Continue the most recent session in this directory
  -r, --resume              Pick a session to resume
  --session <id>            Open a session by id or file
  -n, --name <name>         Name this session
  --model <mode>            Agent mode: light, pro, or ultra
  --list-models             List the agent modes your workspace offers
  --tools <names>           Allow only these local tools (comma-separated)
  --exclude-tools <names>   Turn off these local tools (comma-separated)
  --tui-mode <mode>         fullscreen (default) or regular
  -a, --approve             Trust this project's local files for this run
  -na, --no-approve         Ignore this project's local files for this run
  -h, --help                Show this help
  -v, --version             Show the agent version

Examples:
  cloudthinker agent
  cloudthinker agent "Why is the api pod restarting?"
  cloudthinker agent -p "Summarize the open alerts"
  cloudthinker agent -p "List the Postgres tables" --mode json
  git diff | cloudthinker agent -p "Review this change"
  cloudthinker agent --tools read,grep,find,ls -p "Review src/ without editing"
  cloudthinker agent -c

Inside the agent, type / for commands: /model, /cloud, /share, /login, /changelog, /bug.
Project instructions come from AGENTS.md or CLAUDE.md in your repository.
`;

export const NO_SESSION_REFUSAL = "cloudthinker agent always keeps a session; --no-session is not supported";
export const MODEL_REFUSAL = "cloudthinker agent runs CloudThinker Agent modes only: pick one with --model light, pro, or ultra (--list-models shows them)";
export const THINKING_REFUSAL = "--thinking is not available: each agent mode carries its own reasoning, so pick a mode with --model light, pro, or ultra";
export const PRINT_NEEDS_PROMPT = "-p needs a prompt: cloudthinker agent -p \"your question\", or pipe one in: echo \"your question\" | cloudthinker agent -p";
export const UPDATE_REFUSAL = "`cloudthinker agent update` is not a command: run `cloudthinker update` to update the CLI and its agent";

const VALUE_FLAGS = new Set(["--session", "--name", "-n", "--tools", "-t", "--exclude-tools", "-xt", "--tui-mode"]);
const SWITCHES = new Set(["--continue", "-c", "--resume", "-r", "--approve", "-a", "--no-approve", "-na", "--version", "-v"]);
const INTERNAL_VALUE_FLAGS = new Set(["--session-dir"]);
const INTERNAL_SWITCHES = new Set(["--no-extensions", "-ne", "--no-skills", "-ns", "--no-context-files", "-nc"]);
const EXTENSION_FLAGS = ["--subagents-workflow-file="];
const PI_COMMANDS = new Set(["install", "remove", "uninstall", "list", "config", "auth", "mcp"]);
const MOVED: Record<string, string> = {
	"--fork": "use /fork inside the agent",
	"--export": "use /export inside the agent",
	"--use-theme": "pick a theme in /settings",
	"--theme": "pick a theme in /settings",
	"--system-prompt": "put project instructions in AGENTS.md",
	"--append-system-prompt": "put project instructions in AGENTS.md",
	"--skill": "skills come from your CloudThinker workspace and .agents/skills",
	"--extension": "extensions are not supported",
	"-e": "extensions are not supported",
};

export type SurfaceCheck = { kind: "run" } | { kind: "help" } | { kind: "refuse"; message: string };

function notAvailable(flag: string): string {
	const hint = MOVED[flag];
	return `${flag} is not available in cloudthinker agent${hint ? `: ${hint}` : ""}. Run cloudthinker agent --help for the options.`;
}

function cloudModel(value: string): boolean {
	if (value.includes(":")) return false;
	const slash = value.indexOf("/");
	return slash === -1 || value.slice(0, slash) === PROVIDER_ID;
}

function cloudModelScope(value: string): boolean {
	return value.split(",").every((pattern) => pattern.trim().startsWith(`${PROVIDER_ID}/`));
}

export function checkSurface(argv: readonly string[], stdinIsTTY: boolean | undefined = process.stdin.isTTY): SurfaceCheck {
	const separator = argv.indexOf("--");
	const flags = separator === -1 ? argv : argv.slice(0, separator);
	if (flags.includes("--help") || flags.includes("-h")) return { kind: "help" };
	const first = argv[0];
	if (first === "update") return { kind: "refuse", message: UPDATE_REFUSAL };
	if (first !== undefined && PI_COMMANDS.has(first)) {
		return { kind: "refuse", message: `\`${first}\` is not a cloudthinker agent command. To ask it as a prompt, run: cloudthinker agent -p "${first} ..."` };
	}
	let print = false;
	let prompt = separator !== -1 && separator < argv.length - 1;
	for (let index = 0; index < flags.length; index += 1) {
		const arg = flags[index] as string;
		const next = flags[index + 1];
		if (arg === "--print" || arg === "-p") {
			print = true;
			if (next !== undefined && !next.startsWith("-")) {
				prompt = true;
				index += 1;
			}
		} else if (arg === "--model") {
			if (next === undefined || !cloudModel(next)) return { kind: "refuse", message: MODEL_REFUSAL };
			index += 1;
		} else if (arg === "--models") {
			if (next === undefined || !cloudModelScope(next)) return { kind: "refuse", message: MODEL_REFUSAL };
			index += 1;
		} else if (arg === "--provider" || arg === "--api-key") {
			return { kind: "refuse", message: MODEL_REFUSAL };
		} else if (arg === "--thinking") {
			return { kind: "refuse", message: THINKING_REFUSAL };
		} else if (arg === "--no-session") {
			return { kind: "refuse", message: NO_SESSION_REFUSAL };
		} else if (arg === "--mode") {
			if (next !== "json" && next !== "text") return { kind: "refuse", message: "--mode takes json or text; use it with -p" };
			index += 1;
		} else if (arg === "--list-models") {
			if (next !== undefined && !next.startsWith("-") && !next.startsWith("@")) index += 1;
		} else if (VALUE_FLAGS.has(arg) || INTERNAL_VALUE_FLAGS.has(arg)) {
			index += 1;
		} else if (SWITCHES.has(arg) || INTERNAL_SWITCHES.has(arg)) {
			continue;
		} else if (EXTENSION_FLAGS.some((prefix) => arg.startsWith(prefix))) {
			continue;
		} else if (arg.startsWith("-")) {
			return { kind: "refuse", message: notAvailable(arg.split("=")[0] as string) };
		} else {
			prompt = true;
		}
	}
	if (print && !prompt && stdinIsTTY) return { kind: "refuse", message: PRINT_NEEDS_PROMPT };
	return { kind: "run" };
}
