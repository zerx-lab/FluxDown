# FluxDown 粘性红线

仅收录 AGENTS.md 未覆盖的硬约束（其余规范见项目根 AGENTS.md，已自动加载）：

- 禁止未经用户明确要求执行 git commit / push / tag；推送 v* tag 会直接触发 GitHub Actions 全平台发布流水线，属不可逆操作。
- 分支模型：`develop` = 开发分支（超集），`main` = 稳定分支（子集）。日常开发一律在 `develop`；禁止直接向 `main` 提交功能。
- `main` 只能通过合并 `develop`（或从 `develop` cherry-pick）前进；hotfix 若直接进 `main`，必须同回合同步回 `develop`。
- 一致性判定：`git log main --not develop --oneline` 必须为空。任何操作后不为空即违规，先修复再继续。
- 稳定 tag `vX.Y.Z` 只从 `main` 打；前沿 tag `vX.Y.Z-rc.N` 只从 `develop` 打。
