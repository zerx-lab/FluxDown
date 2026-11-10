/** 与 check_bash.mjs 对齐的类型声明（供 omp guard.ts 引用；勿放进 hooks/pre，omp 会当扩展加载）。 */

export type Verdict = { block: true; reason: string } | { confirm: string } | undefined;

export interface Caller {
	/** 子代理（task / eval agent / 克隆会话 / advisor）。 */
	isSub: boolean;
	/** 小写的子代理定义名，例如 `build-verifier`。 */
	name: string;
}

export function checkBash(
	line: string,
	caller: Caller,
	stableDiverged?: (dir?: string) => string | undefined,
	releaseBlockersFn?: (tag: string, dir?: string) => string[],
): Verdict;

export function stableDivergence(cwd: string): string | undefined;

export function releaseBlockers(cwd: string, tag: string): string[];
