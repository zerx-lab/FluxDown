/**
 * FluxDown bash 命令守卫的纯判定逻辑（零 omp / Cursor SDK 依赖）。
 * omp `hooks/pre/guard.ts` 与 Cursor `.cursor/hooks/omp-guard.mjs` 共用。
 * 放在 `hooks/lib/`（不是 `hooks/pre/`）：omp 会把 `pre/` 下每个文件当扩展工厂加载。
 *
 * @typedef {{ block: true, reason: string } | { confirm: string } | undefined} Verdict
 * @typedef {{ isSub: boolean, name: string }} Caller
 */

import { execFileSync } from "node:child_process";

const RELEASE_TAG = /^(?:\+?)(?:refs\/tags\/)?v\d/;
const PROTECTED = /^(?:\+)?(?:refs\/heads\/)?(?:main|stable)$/;
const CARGO_HEAVY = /\bcargo\s+(?:\+\S+\s+)?(build|b|test|t|nextest|clippy|run|r|bench|doc)\b/;
const MOBILE_BUILD = /(?:^|[\s/])(?:gradlew|xcodebuild|build-core\.sh|testflight\.sh|sideload\.sh|package\.sh)\b/;

/** 引号内的词：带此前缀后永远不会被当作选项或 refspec 识别。 */
const QUOTED = "\u0000";

/**
 * 拆成简单命令的词列表（策略判定足够，不是完整 shell 解析器）。
 * @param {string} line
 * @returns {string[][]}
 */
function commands(line) {
	const out = [];
	let words = [];
	let word = "";
	let quoted = false;
	let quote = undefined;
	const endWord = () => {
		if (word || quoted) words.push(quoted ? QUOTED + word : word);
		word = "";
		quoted = false;
	};
	const endCommand = () => {
		endWord();
		if (words.length) out.push(words);
		words = [];
	};
	for (let i = 0; i < line.length; i++) {
		const c = line[i];
		if (quote) {
			if (c === quote) quote = undefined;
			else if (c === "\\" && quote === '"' && i + 1 < line.length) word += line[++i];
			else word += c;
		} else if (c === "'" || c === '"') {
			quote = c;
			quoted = true;
		} else if (c === "\\" && i + 1 < line.length) {
			word += line[++i];
		} else if (c === ";" || c === "\n" || c === "|" || c === "&") {
			endCommand();
		} else if (/\s/.test(c)) {
			endWord();
		} else {
			word += c;
		}
	}
	endCommand();
	return out;
}

/** @type {Record<string, true>} */
const GIT_VALUE_OPTS = {
	"-m": true,
	"--message": true,
	"-F": true,
	"--file": true,
	"--author": true,
	"--date": true,
	"-C": true,
	"-c": true,
	"-t": true,
	"--template": true,
};

/**
 * @param {string[]} words
 * @returns {{ args: string[], dir?: string } | undefined}
 */
function gitArgs(words) {
	const at = words.findIndex(w => !/^[A-Za-z_][A-Za-z0-9_]*=/.test(w) && !["env", "command", "sudo"].includes(w));
	if (at < 0 || !(words[at] === "git" || words[at].endsWith("/git"))) return undefined;
	let dir = undefined;
	let i = at + 1;
	while (i < words.length && words[i].startsWith("-")) {
		if (words[i] === "-C") dir = words[i + 1]?.replace(QUOTED, "");
		i += words[i] === "-C" || words[i] === "-c" ? 2 : 1;
	}
	const [sub, ...rest] = words.slice(i);
	const args = rest.map((w, k) => (k > 0 && GIT_VALUE_OPTS[rest[k - 1]] ? QUOTED + w : w));
	return { args: sub === undefined ? [] : [sub, ...args], dir };
}

/** @param {string} refspec */
function destination(refspec) {
	const colon = refspec.lastIndexOf(":");
	return colon >= 0 ? refspec.slice(colon + 1) : refspec;
}

/** @param {string} refspec */
function releaseTagName(refspec) {
	const spec = refspec.replace(/^\+/, "");
	const colon = spec.lastIndexOf(":");
	const name = (ref) => ref.replace(/^refs\/tags\//, "");
	const dst = name(colon >= 0 ? spec.slice(colon + 1) : spec);
	const src = colon >= 0 ? name(spec.slice(0, colon)) : dst;
	return src === dst ? dst : undefined;
}

/**
 * @param {string[]} rest
 * @param {() => string | undefined} stableDiverged
 * @param {(tag: string) => string[]} releaseBlockers
 * @returns {Verdict}
 */
function checkPush(rest, stableDiverged, releaseBlockers) {
	const flags = rest.filter(a => a.startsWith("-"));
	const specs = rest
		.filter(a => !a.startsWith("-"))
		.slice(1)
		.map(a => a.replace(QUOTED, ""));
	const refs = specs.map(destination);
	const lease = flags.some(a => a.startsWith("--force-with-lease"));
	const force = flags.some(a => a === "--force" || a === "-f" || /^-[a-zA-Z]*f[a-zA-Z]*$/.test(a));
	const plusForce = specs.some(r => r.startsWith("+"));
	const unresolved = refs.some(r => /[$`]/.test(r));

	if ((force || plusForce || lease) && refs.some(r => PROTECTED.test(r))) {
		return { block: true, reason: "禁止强推 main / stable（.omp/RULES.md 分支模型）。" };
	}
	if ((force || plusForce) && !lease) {
		return { block: true, reason: "只允许 --force-with-lease，禁止裸 --force / +refspec 强推。" };
	}
	if (refs.some(r => /^(?:refs\/heads\/)?stable$/.test(r))) {
		const diverged = stableDiverged();
		if (diverged) {
			return {
				block: true,
				reason: `stable 含有 main 没有的提交（git log stable --not main 非空），先同步回 main 再推送：\n${diverged}`,
			};
		}
	}

	const bulk = flags.some(a => a === "--tags" || a === "--follow-tags" || a === "--mirror");
	const tagSpecs = specs.filter(s => RELEASE_TAG.test(destination(s)));
	if (!bulk && !unresolved && tagSpecs.length === 0) return undefined;
	const blockers =
		bulk || unresolved
			? ["--tags / --follow-tags / --mirror 或变量 refspec 无法在执行前校验，改用字面 tag 名：`git push origin vX.Y.Z`"]
			: tagSpecs.flatMap(spec => {
					const tag = releaseTagName(spec);
					if (!tag) return [`${spec}：只能按同名本地 tag 推送（\`git push origin vX.Y.Z\`）才能校验演练`];
					return releaseBlockers(tag).map(b => `${tag}：${b}`);
				});
	if (blockers.length === 0) return undefined;
	return {
		confirm:
			"推送 v* tag 会立即触发全平台发布流水线，不可逆（.omp/RULES.md）。未满足自动放行条件" +
			`（tag 提交上全量 CI 与同名演练都 success）：\n- ${blockers.join("\n- ")}`,
	};
}

/**
 * 判定一条 bash 命令。
 * @param {string} line
 * @param {Caller} caller
 * @param {(dir?: string) => string | undefined} [stableDiverged]
 * @param {(tag: string, dir?: string) => string[]} [releaseBlockersFn]
 * @returns {Verdict}
 */
export function checkBash(
	line,
	caller,
	stableDiverged = () => undefined,
	releaseBlockersFn = () => ["未接入演练校验"],
) {
	for (const words of commands(line)) {
		const git = gitArgs(words);
		if (git) {
			const [sub, ...rest] = git.args;
			const noVerify =
				rest.some(a => a === "--no-verify") ||
				(sub === "commit" && rest.some(a => /^-[a-zA-Z]*n[a-zA-Z]*$/.test(a)));
			if (noVerify && ["commit", "merge", "push", "rebase", "cherry-pick", "am", "revert"].includes(sub ?? "")) {
				return {
					block: true,
					reason: "禁止 --no-verify 绕过本地 hook（AGENTS.md §2）。修复 hook 报告的问题（如 cargo fmt 后重新 add）再提交。",
				};
			}
			if (sub === "push") {
				const verdict = checkPush(
					rest,
					() => stableDiverged(git.dir),
					tag => releaseBlockersFn(tag, git.dir),
				);
				if (verdict) return verdict;
			}
			continue;
		}

		const cmd = words.filter(w => !w.startsWith(QUOTED)).join(" ");
		const cargo = CARGO_HEAVY.exec(cmd);
		if (cargo && /\s--workspace\b/.test(cmd) && /^(?:test|t|nextest)$/.test(cargo[1])) {
			return {
				block: true,
				reason: "测试禁止 --workspace（AGENTS.md §2）：用 `cargo nextest run -p <crate> <filter>` 精准跑。",
			};
		}

		if (caller.isSub && caller.name !== "build-verifier") {
			const wholeWorkspace = cargo && (/\s--workspace\b/.test(cmd) || !/\s(?:-p|--package)[\s=]/.test(cmd));
			if (wholeWorkspace || MOBILE_BUILD.test(cmd)) {
				return {
					block: true,
					reason:
						"子代理只跑自己改动 crate 的精准命令（`cargo check|clippy|nextest run -p <crate>`）。" +
						"全量 clippy、无 -p 的 cargo 构建/测试与 gradlew / xcodebuild / build-core.sh 由主会话或 build-verifier 统一跑一次；" +
						"在报告里写明需要跑哪些命令。",
				};
			}
		}
	}
	return undefined;
}

/**
 * @param {string} cwd
 * @returns {string | undefined}
 */
export function stableDivergence(cwd) {
	try {
		const out = execFileSync("git", ["-C", cwd, "log", "stable", "--not", "main", "--oneline"], {
			encoding: "utf8",
			stdio: ["ignore", "pipe", "ignore"],
			timeout: 10_000,
		}).trim();
		return out || undefined;
	} catch {
		return undefined;
	}
}

/**
 * @param {string} json
 */
function parseRuns(json) {
	const body = JSON.parse(json);
	if (!body || typeof body !== "object" || !("workflow_runs" in body) || !Array.isArray(body.workflow_runs)) {
		throw new Error("返回体缺少 workflow_runs");
	}
	return body.workflow_runs.map(run => {
		if (
			run &&
			typeof run === "object" &&
			"display_title" in run &&
			typeof run.display_title === "string" &&
			"status" in run &&
			typeof run.status === "string" &&
			"conclusion" in run &&
			(typeof run.conclusion === "string" || run.conclusion === null) &&
			"created_at" in run &&
			typeof run.created_at === "string" &&
			"html_url" in run &&
			typeof run.html_url === "string"
		) {
			return {
				display_title: run.display_title,
				status: run.status,
				conclusion: run.conclusion,
				created_at: run.created_at,
				html_url: run.html_url,
			};
		}
		throw new Error("workflow run 字段不符合预期");
	});
}

/**
 * 发布 tag 自动放行校验，与 `.github/scripts/release_gate.py` 同口径。
 * @param {string} cwd
 * @param {string} tag
 * @returns {string[]}
 */
export function releaseBlockers(cwd, tag) {
	const exec = (cmd, args) =>
		execFileSync(cmd, args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 20_000 }).trim();
	let sha;
	try {
		sha = exec("git", ["rev-parse", "--verify", "--quiet", `refs/tags/${tag}^{commit}`]);
	} catch {
		return ["本地不存在该 tag，先 `git tag -a` 再推送"];
	}
	const title = `Release 演练 ${tag}（不发布）`;
	const latest = (workflow, query, only) => {
		const out = exec("gh", ["api", `repos/{owner}/{repo}/actions/workflows/${workflow}/runs?head_sha=${sha}${query}`]);
		const runs = parseRuns(out).filter(r => only === undefined || r.display_title === only);
		runs.sort((a, b) => b.created_at.localeCompare(a.created_at));
		return runs[0];
	};
	const unmet = (label, run) => {
		if (!run) return `${label}：提交 ${sha.slice(0, 10)} 上没有运行`;
		if (run.status !== "completed") return `${label} 仍在运行（${run.html_url}）`;
		if (run.conclusion !== "success") return `${label} 结论为 ${run.conclusion}（${run.html_url}）`;
		return undefined;
	};
	try {
		return [
			unmet("全量 CI（ci.yml）", latest("ci.yml", "&per_page=20")),
			unmet(`演练「${title}」`, latest("release.yml", "&event=workflow_dispatch&per_page=50", title)),
		].filter(b => b !== undefined);
	} catch (e) {
		return [`无法查询 GitHub Actions：${e instanceof Error ? e.message : String(e)}`];
	}
}
