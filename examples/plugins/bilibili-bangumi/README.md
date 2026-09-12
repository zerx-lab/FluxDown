# Bilibili 番剧插件

独立的 FluxDown `.fxplug` 示例插件，不依赖 `fluxdown@ytdlp`，也不声明
`flux.ytdlp` 权限。

## 当前能力

- 识别 `bangumi/play/ss...` 和 `bangumi/media/md...` 番剧链接。
- 调用 Bilibili 番剧接口获取正片分集清单。
- 可选包含 PV、采访和其他番外分区。
- 通过 `resolverItem=ep:<id>` 二次获取单集播放地址。
- 直接消费 Bilibili DASH 视频/音频流，支持画质变体和仅音频。
- 仅支持在插件设置中导入 Bilibili Cookie。
- 支持 Cookie 头和 Netscape `cookies.txt`。
- 手动导入的 Cookie 会保存到插件专属持久化存储；只要 Cookie 仍有效，后续解析会直接复用。
- 插件不会主动检查或强制要求登录；公开内容可直接解析，会员内容由 Bilibili 接口自行判断。

## 当前边界

插件系统目前没有后台定时器或主动创建任务的 API，因此这个包负责“分集发现
和单集解析”，不负责长期订阅轮询。订阅能力需要未来由宿主调度，或由独立
RSS/daemon 负责。

Cookie 可以在有效期内重复使用，但不能从 Cookie 反推出密码或刷新令牌；过期后需要
重新导入有效 Cookie。登录态逻辑完全位于插件脚本中，客户端只负责显示普通 Cookie
设置项；插件不修改客户端或引擎设置。
