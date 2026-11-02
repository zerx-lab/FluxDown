#!/usr/bin/env node
/**
 * Cursor beforeShellExecution 薄壳：调用 `.omp/hooks/lib/check_bash.mjs` 共享判定。
 * 正文与策略不在本文件维护——改规则改 `.omp/hooks/lib/check_bash.mjs`。
 */
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import {
	checkBash,
	releaseBlockers,
	stableDivergence,
} from "../../.omp/hooks/lib/check_bash.mjs";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

/**
 * @returns {Promise<unknown>}
 */
async function readStdinJson() {
	const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
	let raw = "";
	for await (const line of rl) raw += line;
	if (!raw.trim()) return {};
	return JSON.parse(raw);
}

/**
 * Cursor hook 输入里尽量识别子代理；字段因版本可能不同，缺省按主会话处理。
 * @param {Record<string, unknown>} input
 */
function callerFrom(input) {
	const sub =
		input.subagent ??
		input.agent ??
		input.subagent_type ??
		(typeof input.composer === "object" && input.composer !== null ? input.composer : undefined);
	if (!sub || typeof sub !== "object") {
		return { isSub: false, name: "" };
	}
	const s = /** @type {Record<string, unknown>} */ (sub);
	const name = String(s.name ?? s.type ?? s.subagent_type ?? "").toLowerCase();
	const kind = String(s.kind ?? s.role ?? "");
	const isSub =
		Boolean(s.isSub) ||
		kind === "sub" ||
		kind === "subagent" ||
		Boolean(name) ||
		input.hook_event_name === "subagentStart";
	return { isSub, name };
}

const input = /** @type {Record<string, unknown>} */ (await readStdinJson());
const command = typeof input.command === "string" ? input.command : "";
if (!command) {
	process.stdout.write(JSON.stringify({ permission: "allow" }));
	process.exit(0);
}

const cwd =
	typeof input.cwd === "string" && input.cwd
		? resolve(repoRoot, input.cwd)
		: typeof input.working_directory === "string" && input.working_directory
			? resolve(repoRoot, input.working_directory)
			: repoRoot;

const verdict = checkBash(
	command,
	callerFrom(input),
	dir => stableDivergence(dir ? resolve(cwd, dir) : cwd),
	(tag, dir) => releaseBlockers(dir ? resolve(cwd, dir) : cwd, tag),
);

if (!verdict) {
	process.stdout.write(JSON.stringify({ permission: "allow" }));
	process.exit(0);
}

if ("block" in verdict) {
	process.stdout.write(
		JSON.stringify({
			permission: "deny",
			user_message: verdict.reason,
			agent_message: verdict.reason,
		}),
	);
	process.exit(0);
}

// 发版 tag 未满足自动放行条件：交给用户确认（Cursor permission: ask）
process.stdout.write(
	JSON.stringify({
		permission: "ask",
		user_message: verdict.confirm,
		agent_message: verdict.confirm,
	}),
);
process.exit(0);
