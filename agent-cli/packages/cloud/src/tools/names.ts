export const CT_SANDBOX_READ = "ct_sandbox_read";
export const CT_SANDBOX_WRITE = "ct_sandbox_write";
export const READ_TASK_OUTPUT = "read_task_output";
export const CT_ASK = "ct_ask";
export const CT_RUN_STATUS = "ct_run_status";
export const TOOL_SEARCH = "tool_search";
export const LOAD_SKILL = "load_skill";

export const CLOUD_TOOLS: string[] = [
	CT_SANDBOX_READ,
	CT_SANDBOX_WRITE,
	READ_TASK_OUTPUT,
	CT_ASK,
	CT_RUN_STATUS,
	TOOL_SEARCH,
	LOAD_SKILL,
];

export function addCloudTools(names: string[]): void {
	for (const name of names) if (!CLOUD_TOOLS.includes(name)) CLOUD_TOOLS.push(name);
}
