// FluxDown 插件通知钩子入口（classic script；入口挂 globalThis）。
//
// 全部钩子均为 fire-and-forget：抛错/超时只记日志，绝不影响任务状态。

globalThis.onStart = async (ctx) => {
  flux.logger.info('[echo] task started:', ctx.taskId, ctx.url);
};

globalThis.onDone = async (ctx) => {
  flux.logger.info('[echo] task done:', ctx.taskId, '->', ctx.filePath);
};
