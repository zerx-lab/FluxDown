// Bilibili 下载通知钩子（classic script；入口挂 globalThis）。
//
// 下载失败时请求延迟重试。重试计数按任务保存，避免一个任务无限重试；
// 计数和间隔可在插件设置中调整，默认最多 5 次、每次间隔 2 秒。
// flux.task.requestRetry 由宿主现有的自动重试上限统一限流。

const DEFAULT_RETRY_COUNT = 5;
const DEFAULT_RETRY_DELAY_SECONDS = 2;

function retryKey(ctx) {
  return `bilibili.retryCount:${String(ctx.taskId || 'unknown')}`;
}

function positiveNumber(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function retryLimit() {
  return Math.floor(
    positiveNumber(flux.settings && flux.settings.retryCount, DEFAULT_RETRY_COUNT),
  );
}

function retryDelayMs() {
  const seconds = positiveNumber(
    flux.settings && flux.settings.retryDelaySeconds,
    DEFAULT_RETRY_DELAY_SECONDS,
  );
  return Math.max(1000, Math.round(seconds * 1000));
}

globalThis.onStart = async (ctx) => {
  // 新一轮手动启动时重新计算重试次数。
  await flux.storage.set(retryKey(ctx), '0');
};

globalThis.onDone = async (ctx) => {
  // 成功后清理计数，避免任务记录长期占用插件存储。
  await flux.storage.set(retryKey(ctx), '0');
};

globalThis.onError = async (ctx) => {
  const limit = retryLimit();
  const key = retryKey(ctx);
  const current = positiveNumber(await flux.storage.get(key), 0);

  if (current >= limit) {
    flux.logger.warn(
      '[bilibili] download failed; retry limit reached:',
      ctx.taskId,
      ctx.message,
    );
    return;
  }

  const next = current + 1;
  const delayMs = retryDelayMs();
  await flux.storage.set(key, String(next));
  flux.logger.warn(
    '[bilibili] download failed; requesting retry',
    `${next}/${limit}`,
    `after ${delayMs}ms:`,
    ctx.taskId,
    ctx.message,
  );
  flux.task.requestRetry({ delayMs });
};
