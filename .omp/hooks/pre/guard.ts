/**
 * FluxDown 命令守卫：把 .omp/RULES.md（git 授权 / 分支模型）与 AGENTS.md §2、§7「并行子代理」里机械可判定的红线在工具调用前硬拦截。
 *
 * TTSR 规则（.omp/rules/forbidden-commands.md）只在输出流里打断并提示，模型可原样重试；
 * 这里在执行前直接拒绝，不可绕过。规则正文仍以 RULES.md / AGENTS.md 为准；纯判定见
 * `.omp/hooks/lib/check_bash.mjs`（Cursor `.cursor/hooks/omp-guard.mjs` 共用；勿把共享模块放进 `pre/`，
 * omp 会把该目录下每个文件当扩展加载）。
 *
 * - 推送 v* tag：tag 提交上最近一次全量 CI（ci.yml）与同名演练（「Release 演练 <tag>（不发布）」）都 success 时
 *   直接放行（用户要求发版即已授权，演练全绿不再人工确认）；任一未满足、或 --tags / --follow-tags / --mirror /
 *   变量 refspec 无法预先解析时，交互会话弹确认并列出未满足项，无 UI 时拒绝。
 * - --no-verify：绕过 .githooks 本地门禁（AGENTS.md §2 明令禁止）。
 * - 裸 --force 推送：只允许 --force-with-lease；main / stable 一律禁止强推。
 * - 推送 stable 前校验分支模型一致性：`git log stable --not main` 必须为空。
 * - `cargo test|nextest --workspace`：测试禁全量（§2）。
 * - 子代理（build-verifier 除外）禁止全量门禁与移动端整包构建：多个子代理共享 target/ 与机器资源，
 *   全量 clippy / 无 -p 的 cargo 构建测试、gradlew、xcodebuild、build-core.sh 由主会话或 build-verifier 跑一次。
 */
import { resolve } from "node:path";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import {
	checkBash,
	releaseBlockers,
	stableDivergence,
	type Caller,
	type Verdict,
} from "../lib/check_bash.mjs";

export type { Caller, Verdict };
export { checkBash, releaseBlockers, stableDivergence };

export default function guard(pi: ExtensionAPI): void {
	pi.on("tool_call", async (event, ctx) => {
		if (event.toolName !== "bash") return;
		const command = event.input.command;
		if (typeof command !== "string") return;
		const cwd = typeof event.input.cwd === "string" && event.input.cwd ? resolve(ctx.cwd, event.input.cwd) : ctx.cwd;
		const caller: Caller = { isSub: ctx.agent?.kind === "sub", name: (ctx.agent?.name ?? "").toLowerCase() };
		const verdict = checkBash(
			command,
			caller,
			dir => stableDivergence(dir ? resolve(cwd, dir) : cwd),
			(tag, dir) => releaseBlockers(dir ? resolve(cwd, dir) : cwd, tag),
		);
		if (!verdict) return;
		if ("block" in verdict) return verdict;
		if (ctx.hasUI && (await ctx.ui.confirm("推送发布 tag？", `${verdict.confirm}\n\n${command}`))) return;
		return { block: true, reason: `${verdict.confirm}\n未获用户确认，已拦截。` };
	});
}
