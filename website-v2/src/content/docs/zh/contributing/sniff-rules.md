---
title: 贡献嗅探规则
description: 把你的嗅探规则分享给所有 FluxDown 用户——维护自己的规则仓库（登记一次，发 Release 自动收录），或一次性提交；以及规则测试的写法。
section: contributing
order: 3
sourceHash: "0df4ef738d9b"
---

嗅探规则的官方规则与社区规则索引存放在公开仓库 [zerx-lab/fluxdown-sniff-rules](https://github.com/zerx-lab/fluxdown-sniff-rules)（下文简称「规则仓」）。对你有效的规则多半对所有人都有效，而且不必等扩展发版：规则被收录后会自动签名，并经桌面端下发。每个规则字段的含义见[嗅探规则参考](/docs/zh/browser-extension/sniff-rules/)。

## 两种贡献方式

| | 维护自己的规则仓库（推荐长期维护者） | 一次性提交（Issue 表单） |
| --- | --- | --- |
| 规则放在哪 | 你自己的公开仓库 | 规则仓的 `rules/community/` |
| 规则 id | `community.<你的命名空间>.<名字>` | `community.misc.<名字>` |
| 首次 | 提一个只改 `registry.json` 的登记 PR，维护者审核 | 提 Issue，机器人开 PR，维护者审核 |
| 之后更新 | **发 Release 即可，一小时内自动收录，不再人工审核** | 每次都要再提 Issue 并审核 |
| 能覆盖的站点 | 只能是你登记时认领的站点（独占） | 任何**未被认领**的站点 |

怎么选：

- 只想给某个站点补一条规则、以后不打算维护 → **一次性提交**，最省事。
- 想长期维护一个或几个站点的规则、会不断调整 → **维护自己的规则仓库**。登记时审核一次，之后你自己掌控更新节奏。
- 想维护的站点已经被别人的仓库认领 → 向对方的仓库提 Pull Request 贡献规则（见[收到他人贡献](#收到他人贡献)），不能再认领。

## 维护自己的规则仓库

整个流程只需要一个 GitHub 账号。顺序是：建仓库 → 写规则 → 本地调通 → 自检 → 发 Release → 登记（一次）→ 以后只发 Release。

### 1. 用模板创建仓库

1. 打开 [zerx-lab/fluxdown-sniff-rules-template](https://github.com/zerx-lab/fluxdown-sniff-rules-template)。
2. 点右上角 **Use this template → Create a new repository**。
3. 仓库**必须是 Public**（规则仓无法读取私有仓库的 Release）。

模板自带示例规则 `sniff-rules.json`、`check`（自检）和 `release`（发布）两个工作流。

### 2. 编辑 `sniff-rules.json`

文件在仓库根目录，结构如下：

```json
{
  "namespace": "alice",
  "hosts": ["example.com", "example-cdn.net"],
  "rules": [
    {
      "id": "community.alice.example-video",
      "version": 1,
      "tier": "community",
      "title": "Example：把 .xyz 文件识别为视频",
      "stage": "response",
      "match": { "host": ["example.com"], "ext": ["xyz"] },
      "action": { "kind": "resource", "type": "video" }
    }
  ],
  "tests": [
    {
      "name": "识别 xyz 视频",
      "call": "classifyResponse",
      "args": [{ "url": "https://cdn.example.com/a.xyz", "pageUrl": "https://example.com/", "size": 0, "attachment": false }],
      "expect": { "decision": "resource", "type": "video" }
    }
  ]
}
```

| 字段 | 要求 |
| --- | --- |
| `namespace` | 必填。2–31 个小写字母 / 数字 / 连字符，以字母或数字开头（例如你的 GitHub 用户名）。**不能**是保留字 `misc`、`official`、`builtin`、`fluxdown`、`community`、`local`、`plugin`、`test`。全规则仓唯一，登记后不能改。 |
| `hosts` | 拟认领的站点，只用于登记前自检；登记后以规则仓 `registry.json` 里的认领为准。 |
| `rules[].id` | 必须是 `community.<namespace>.<名字>`，全规则仓唯一。 |
| `rules[].tier` | 必须是 `community`。 |
| `rules[].version` | 正整数；改动已发布的规则时要加一（见[更新](#更新)）。 |
| 站点范围 | 规则里出现的每个 `host` / `pageHost`（包括 `exclude` 里的）和 `mirror.hosts`，都必须落在认领站点（含其全部子域）内。 |
| 产出地址 | `extract` / `dom` 规则产出的下载地址，在扩展里只接受域名落在该规则自身 `host` / `pageHost` 范围内的，其它会被丢弃。 |
| `tests` | 可选但强烈建议，写法见[编写规则测试](#编写规则测试)。 |

其余规则字段与社区层的限制（必须写 `host` 或 `pageHost`、不能用 `preempt`、不能用全局 `strip-params` 等）见[嗅探规则参考](/docs/zh/browser-extension/sniff-rules/#各层级的限制)。

### 3. 先在扩展里调通，再改 id

最省事的做法：先在扩展 **设置 → 嗅探规则 → 本地规则** 里写好并用规则测试台验证，再把规则 JSON 粘进 `sniff-rules.json`，并：

1. 把 `id` 从 `local.xxx` 改成 `community.<你的命名空间>.xxx`；
2. 把 `tier` 从 `local` 改成 `community`；
3. 补上 `host` / `pageHost`（社区规则必须写）。

### 4. 自检

推送到 GitHub 后，模板里的 `check` 工作流会在每次推送时自动运行，做的检查和收录时完全相同（它调用规则仓的 `author-check` 可复用工作流）。也可以在本地运行，需要 Node.js 22 或更高版本，无需安装依赖：

```bash
git clone https://github.com/zerx-lab/fluxdown-sniff-rules
node fluxdown-sniff-rules/scripts/check-source.mjs --file sniff-rules.json --repo <你的用户名>/<仓库名>
```

通过会输出 `✅ 通过：N 条规则、M 条测试`；否则逐条列出问题。尚未登记时，脚本按文件里的 `hosts` 自检，并检查是否与已登记的认领冲突。

### 5. 发布 Release

规则仓只读取你仓库的 **Latest Release** 里名为 `sniff-rules.json` 的附件。

- **推荐：推送 `v*` 标签。** 模板的 `release` 工作流会先自检，通过后创建 Release 并上传 `sniff-rules.json`：

  ```bash
  git tag v1.0.0
  git push origin v1.0.0
  ```

- **手动发布**也可以：在 GitHub 网页创建 Release，附件名必须是 `sniff-rules.json`，**不能是草稿或预发布**。

### 6. 登记（只需一次）

1. Fork [zerx-lab/fluxdown-sniff-rules](https://github.com/zerx-lab/fluxdown-sniff-rules)。
2. 在你的 Fork 里编辑 `registry.json`，在 `sources` 数组末尾追加你的条目：

   ```json
   { "ns": "alice", "repo": "alice/fluxdown-rules", "hosts": ["example.com", "example-cdn.net"], "description": "Example 站点的视频识别" }
   ```

3. 向规则仓提 Pull Request。**这个 PR 只能修改 `registry.json`**，改了别的文件会被机器人判为不通过。

| 字段 | 要求 |
| --- | --- |
| `ns` | 与 `sniff-rules.json` 的 `namespace` 相同；格式与保留字同上。 |
| `repo` | `owner/name`；一个仓库只能登记一个命名空间。 |
| `hosts` | 认领的站点：普通域名，自动包含全部子域；不能带 `=`、`*`、协议、路径；至少两段（`com` 不行）；最多 32 个；不能与他人已有的认领重叠。 |
| `description` | 可选，不超过 200 字。 |

提交后：

1. `registry-check` 机器人会拉取你仓库的 Latest Release，跑与收录完全相同的检查，并在 PR 里评论结果：通过时列出规则清单（id、版本、阶段、标题）；不通过时列出需要修正的问题。修好后向 PR 推送新提交（或在你的仓库发新 Release 后重新运行检查）即可重新检查。
2. 如果 PR 作者不是 `repo` 的 owner，机器人会提示维护者确认已获仓库所有者同意；如果是**更换已登记命名空间的仓库**，需要原维护者在 PR 中确认。
3. 维护者审核**认领范围是否合理**（见[审核与安全边界](#审核与安全边界)）后合并。
4. 合并后，规则仓**一小时内**收录你的 Release。

### 更新

登记之后的更新不再需要 PR 或人工审核：

1. 修改 `sniff-rules.json`。**每条改动过的规则 `version` 加一。**
2. 推送新标签（如 `v1.0.1`）发布新 Release。
3. 规则仓每小时（整点后第 17 分钟）收录一次，签名后发布。
4. 生效时间链：规则仓收录签发 **≤ 1 小时**，桌面端每 6 小时检查一次规则仓，扩展每 6 小时向桌面端同步一次。所以从发 Release 到用户的扩展里生效，通常**最多十几个小时**。扩展的**嗅探规则**页点**立即同步**可以立刻拉取桌面端已有的版本。

用户只有在**启用社区规则**开启、并且桌面端在运行时才会收到社区规则。

### 收录失败与拒收

收录失败时，规则仓会开一个标题为 `stage-fail: <你的命名空间>` 的 issue 并 @ 仓库 owner；**已收录的旧版本继续生效**。修好后发新 Release，下一次收录成功时 issue 自动关闭。

以下情况会被拒收：

- **替换已收录 Release 的附件**（重新上传同名附件）——会被判为篡改。永远发新的 Release，不要改旧的。
- **最新 Release 不比已收录的新**（发布时间不晚于已收录的那个）。
- **改了规则内容但 `version` 没有变大。**
- **站点超出认领**：规则的 `host` / `pageHost` / `mirror.hosts` 不在登记的认领范围内。
- **规则校验不通过**，或 **规则测试失败**（测试与规则仓全部规则一起运行，见下文）。
- Release 里没有名为 `sniff-rules.json` 的附件，或 Release 是草稿 / 预发布。

### 扩大认领、更换仓库、停止维护

都是向规则仓提 PR 修改 `registry.json` 里你自己的条目：

- **扩大认领**：在 `hosts` 里加站点。维护者会审核新增的站点是否合理，且不能与他人的认领重叠。
- **更换仓库**：改 `repo`。需要原维护者在 PR 中确认。
- **停止维护**：删除你的条目。合并后收录任务会下架，你的规则从社区包中移除。

### 收到他人贡献

你的规则仓库就是普通的 GitHub 仓库：别人想改进规则时，会向**你的仓库**提 Pull Request。你审核、合并后，按[更新](#更新)发新 Release 即可（别忘了 `version` 加一）。规则仓不会参与这个过程，也不会自动合并任何东西。

## 一次性提交

不想建仓库时用这条路。规则会以 `community.misc.<名字>` 存进规则仓的 `rules/community/<站点>.json`。

1. **写一条本地规则并验证。** 设置页 → **嗅探规则** → **本地规则** → **新建**；用同一面板里的规则测试台填入资源地址，确认结论符合预期，也测一个**不该**命中的地址。
2. **在规则行点「提交到社区」。** 扩展会先按社区规则的要求校验，并把 `local.x` 改写为 `community.misc.x`；通过后打开预填好的 GitHub Issue。如果规则太长、塞不进链接，扩展会改为把规则复制到剪贴板并打开空表单，请粘贴到「规则 JSON」一栏。
3. **填写 Issue**（见下表）并提交。
4. **机器人自动校验**：规则校验、规则测试，以及站点是否已被登记仓库认领。
   - 未通过：机器人回复原因，**编辑 Issue** 即可自动重新校验。
   - 通过：机器人开一个 Pull Request 并回复链接。
5. **维护者审核 Pull Request**（见[审核与安全边界](#审核与安全边界)）。
6. **合并后自动签名发布**，生效时间同样受「桌面端 6 小时 + 扩展 6 小时」的同步间隔影响。

**已被登记仓库认领的站点不接受一次性提交**，机器人会提示你向对应仓库贡献。

### Issue 表单

| 字段 | 必填 | 填什么 |
| --- | --- | --- |
| 规则 JSON | 是 | 按钮会自动预填。也可以粘贴单条规则、规则数组，或导出的规则文件 / `{ "rules": [...], "tests": [...] }`。名为 `local.xxx` 的规则会自动改成 `community.misc.xxx`，其它层级会被拒绝。 |
| 示例页面 | 是 | 规则生效的一个真实页面的完整 `http(s)` 地址。如果它的域名不在任何一条规则的 `host` / `pageHost` 范围内，机器人会给出提醒。 |
| 示例资源地址 | 否 | 规则应该识别（或忽略）的资源地址，每行一个，供审核参考。 |
| 测试用例 | 否 | 测试的 JSON 数组（格式见下文），会被加入规则文件。带测试的规则审核更快。 |
| 说明 | 否 | 规则解决什么问题，为什么这样写。 |
| 确认 | 是 | 勾选表示你已在本地规则里验证过该规则，并且站点范围尽量小。 |

机器人会把规则写入 `rules/community/<站点>.json`，文件名取第一条规则的第一个 `host` / `pageHost`（去掉 `=` 和 `www.`）。如果该文件已存在：`id` 相同的规则会被替换（内容有变化时，`version` 至少取「原版本 + 1」），新的规则追加，`name` 相同的测试被替换。之后每次修改都要再提一次 Issue 并重新审核。

## 审核与安全边界

**登记时人工审核。** 维护者审核你认领的站点范围是否合理；机器人的评论里会列出你仓库当前所有规则供参考。

**之后自动收录，靠下面这些约束保证安全：**

- **命名空间**：你的规则 id 必须以 `community.<你的命名空间>.` 开头，不能冒用他人或保留字。
- **独占认领**：规则里的每个 `host` / `pageHost` / `mirror.hosts` 都必须落在你登记的认领范围内；认领之间不允许重叠；一次性提交（`misc`）也不能触碰已被认领的站点。
- **产出地址限制**：社区规则的 `extract` / `dom` 产出的下载地址，域名必须落在该规则自身 `host` / `pageHost` 范围内，否则扩展丢弃——规则无法把用户引向它没有声明的站点。社区规则还不能用 `preempt` 和全局 `strip-params`。
- **版本不可变**：已收录的 Release 附件被替换会被拒收；改了规则内容但 `version` 没升也会被拒收；最新 Release 不比已收录的新同样拒收。
- **统一校验与测试**：收录前用扩展同款校验器检查全部规则，并运行全部规则测试，任何一项失败都不收录。
- **统一签名**：作者不持有任何密钥；规则仓统一签名发布，桌面端验签后才下发。
- **吊销与下架**：有问题的规则可通过 `revocations.json` 吊销（任何层级）；从 `registry.json` 删除条目会下架整个命名空间。

## 编写规则测试

每条测试调用规则引擎的一个函数，并把结果与 `expect` 比较。引擎由**内置规则 + 官方规则 + 所有社区规则（所有登记仓库的规则与 `misc` 规则）+ 吊销列表**编译而成。**你的测试会与规则仓全部规则一起运行**：收录前整体通过才会发布，所以也请避免写出会被别人规则影响的测试，并确保你的规则不会让别人的测试失败。

| 字段 | 含义 |
| --- | --- |
| `name` | 必填；建议在同一个规则文件内保持唯一，机器人会替换同名的测试。 |
| `call` | 下表中的函数之一。 |
| `args` | 函数参数组成的数组，按顺序排列。 |
| `expect` | 期望的结果。 |
| `options` | 可选：`{ "sniffImages": true }` 表示本条测试开启图片嗅探（默认关闭）。 |

**比较规则。** `expect` 里的对象只比较你写出来的键（结果里多出的键没关系）；数组必须**长度相同**并逐项比较；其它值（字符串、数字、`null`、布尔）必须完全相等。由于引擎里同时包含内置和官方规则，返回列表的调用（`domRules`、`bodyWatchers`、`extractFromBody`、`mirrorGroups` 等）的结果里可能有其它规则的条目——请选择只会被你的规则处理的地址，并把列表写完整。

| `call` | `args` | 结果 |
| --- | --- | --- |
| `classifyResponse` | `[{ url, pageUrl, size, attachment, mime?, status?, resourceType?, headers? }]`（`size` 和 `attachment` 为必填项；`size`、`-1` 或 `0` = 未知；`headers` 为对象） | `{ decision: "resource", type, confidence?, rule: { id, version, tier } }`、`{ decision: "ignore", rule }` 或 `{ decision: "none" }` |
| `classify` | `[url, mime?]` | 一种资源类型，例如 `"video"` |
| `isIgnored` | `[url, pageUrl, attachment, mime?]`（传递 `mime` 以按与 `classifyResponse` 相同的方式应用基于 MIME 的 `exclude`） | 匹配规则引用 `{ id, version, tier }`，或 `null` |
| `dedupeKey` | `[url, pageUrl]` | 用于检测重复项的规范化 URL 字符串 |
| `fileExtension` | `[url, pageUrl]` | 扩展 (`"zip"`)，或 `""` |
| `trackKind` | `[{ url, pageUrl, filename?, mime? }]` | `"video"`、`"audio"` 或 `null` |
| `bodyWatchers` | `[pageUrl]` | 一个 `{ rule, url?, host?, pathRegex? }` 的列表 |
| `extractFromBody` | `[apiUrl, pageUrl, json]` | 一个 `{ rule, emits: [{ mode, url, filename?, type?, quality? }] }` 的列表 |
| `manifestsFromBody` | `[apiUrl, pageUrl, json]` | 被视为 DASH 清单的 JSON 子树列表 |
| `domRules` | `[pageUrl]` | 一个 `{ rule, selector, attr, emit }` 的列表 |
| `pageHints` | `[pageUrl]` | `{ titles: [{ selector, attr? }], players: [{ selector }] }` |
| `requestHeaders` | `[url, pageUrl]` | 一个由小写头部名称 → 值组成的对象 |
| `mirrorGroups` | `[]` | 主机列表的列表 |

示例 1——一条 `response` 规则和一条反向测试（第二条用来确保规则不会波及其它网站）：

```json
{
  "tests": [
    {
      "name": "example.com 上的 xyz 是视频",
      "call": "classifyResponse",
      "args": [{ "url": "https://cdn.example.com/a.xyz", "pageUrl": "https://example.com/", "size": 0, "attachment": false }],
      "expect": { "decision": "resource", "type": "video", "rule": { "id": "community.example.video" } }
    },
    {
      "name": "其它站点不受影响",
      "call": "classifyResponse",
      "args": [{ "url": "https://cdn.other.com/a.xyz", "pageUrl": "https://other.com/", "size": 0, "attachment": false }],
      "expect": { "decision": "none" }
    }
  ]
}
```

示例 2——一条 `extract` 规则（API 响应作为第三个参数）：

```json
{
  "tests": [
    {
      "name": "提取直链",
      "call": "extractFromBody",
      "args": [
        "https://api.example.com/play?id=1",
        "https://example.com/watch/1",
        { "code": 0, "data": { "url": "https://cdn.example.com/v/1.mp4", "title": "第 1 集" } }
      ],
      "expect": [
        {
          "rule": { "id": "community.example.play-api" },
          "emits": [{ "mode": "resource", "url": "https://cdn.example.com/v/1.mp4", "filename": "第 1 集" }]
        }
      ]
    },
    {
      "name": "失败响应不产出",
      "call": "extractFromBody",
      "args": ["https://api.example.com/play?id=1", "https://example.com/watch/1", { "code": 1 }],
      "expect": []
    }
  ]
}
```

## 撤回规则与官方规则

- **撤回规则**：错误或有害的规则通过规则仓根目录的 `revocations.json` 撤回，而不是悄悄删除。每条记录写明 `ruleId`、影响哪些 `versions`（`"*"` 或 `{ "max": N }`）、是 `soft` 还是 `hard` 吊销，以及 `reason`；用户会看到什么见[吊销](/docs/zh/browser-extension/sniff-rules/#吊销)。请在规则仓提交 Issue 或 Pull Request 描述问题，由维护者处理。你自己仓库里的规则出了问题，更简单的办法是修正后发新 Release（`version` 加一）。
- **官方规则**（`rules/official/`）由维护者维护，可使用 `preempt`、全局 `strip-params` 等特权。

## 发布是怎么进行的

规则仓的 `publish` 工作流**每小时**运行一次（`main` 上规则源变化时也会运行）：先收录登记仓库的新 Release，再校验全部规则、运行测试，最后对官方包、社区包和吊销列表签名。每份签名文档带有只增不减的 `sequence`；桌面端验证签名，并忽略比自己已有版本更旧的内容。社区包和吊销列表有效期 30 天，剩余不足 14 天时自动续签，所以即使没有任何改动也不会过期。签名私钥只保存在规则仓的 Secret 中。
