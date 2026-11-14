/**
 * Reserve the mini-player's close/expand control strip above FluxDown's
 * floating download badge. Return null if the remaining video is too short:
 * hiding the badge is preferable to covering YouTube's Close button.
 *
 * Values are viewport Y coordinates, matching getBoundingClientRect().
 */
export function youtubeMiniplayerSafeTop(
  videoTop: number,
  videoBottom: number,
  badgeHeight: number,
  closeBottom: number,
  margin = 8,
): number | null {
  const miniplayerHeaderHeight = 52;
  const top = Math.max(videoTop + miniplayerHeaderHeight, closeBottom + margin);
  return top + badgeHeight + margin <= videoBottom ? top : null;
}
