---
title: BitTorrent
description: 内置 magnet 与 .torrent 下载,支持 DHT、UPnP、内置 Tracker 与文件选择。
section: protocols
order: 2
sourceHash: "ff770ea58628"
---

FluxDown 内置了 BitTorrent 引擎(基于 librqbit)——不需要另外装一个 BT 客户端。它同时支持 magnet 链接与 `.torrent` 文件,默认开启 DHT 和 UPnP,自带一份精选的公共 Tracker 列表,多文件种子还能在下载开始前逐个选择文件。

## 添加种子

- **Magnet 链接**——像粘贴普通 URL 一样,把 `magnet:` 链接粘贴进新建下载对话框即可。
- **.torrent 文件**——从新建下载对话框的文件选择器打开,或者在*设置 → 通用 → 关联 .torrent 文件*里把 `.torrent` 关联给 FluxDown。关联之后,双击系统里任意位置的种子文件都会直接打开 FluxDown 并开始下载。(仅限 Windows;只写入当前用户的注册表关联,不需要管理员权限。)

<!-- TODO(screenshot): 设置 → 通用,"关联 .torrent 文件"开关 -->

## 选择文件

多文件种子在下载开始前会展示种子内每个文件及其大小的勾选列表——取消勾选不需要的文件,FluxDown 只下载你选中的那些。

<!-- TODO(screenshot): 新建下载对话框中多文件种子的文件选择列表 -->

## DHT、UPnP 与监听端口

节点发现开箱即用:**DHT**(无 Tracker 也能发现对等节点)与 **UPnP 端口映射**(自动配置路由器端口转发)由引擎始终开启,无需配置。*设置 → BitTorrent* 中唯一的连通性选项是用于接收 BT 连接的**监听端口范围**(默认 **6881–6891**)。

所有 BitTorrent 任务共用同一个底层会话——一张 DHT 路由表、一组 Tracker 连接、一个监听端口——而不是每个任务各开一份,这样即便同时跑多个种子,资源占用也能保持较低。

## Tracker

FluxDown 内置 **25 个公共 Tracker**,经过精选以获得较好的全球覆盖:优先 CN/亚洲地区的 Tracker(如果你在中国大陆或周边地区,能获得更好的节点定位),其次是国际 Tracker,并保留了几个 HTTPS 备用地址供屏蔽 UDP 的网络环境使用。这些 Tracker 会自动用于每一个种子——包括没有自带 Tracker 的 magnet 链接——除非你自行覆盖它们。

- **自定义 Tracker**——在*设置 → BitTorrent → Tracker 列表*里粘贴你自己的地址列表(每行一个)来替换内置列表。
- **Tracker 订阅**——*设置 → BitTorrent → Tracker 订阅*会定期抓取社区维护的 Tracker 列表(默认来自 `trackerslist.com` 和 `ngosang/trackerslist`,每 24 小时刷新一次),与你正在使用的列表合并去重后一起生效。

<!-- TODO(screenshot): 设置 → BitTorrent,Tracker 列表与 Tracker 订阅面板 -->

## 断点续传

种子下载依靠 BitTorrent 自身基于分片(piece)的校验机制续传:关闭 FluxDown、断电或崩溃都不会丢失已校验过的分片——只有崩溃时正在传输中的那一片需要重新获取。

## 防吸血保护

FluxDown 会观察对端的吸血行为——反复下载同样的数据、伪造进度、已知吸血客户端——并可对其观察、限速、停止上传或封禁。所有规则默认处于**仅观察**模式,私有种子与局域网对端豁免,并提供分层封禁名单与签名云名单。详见 [BT 防吸血](/docs/zh/protocols/bt-guard/)。

## 相关设置

| 设置项 | 位置 |
|---|---|
| 监听端口范围 | 设置 → BitTorrent |
| Tracker 列表 | 设置 → BitTorrent |
| Tracker 订阅 | 设置 → BitTorrent |
| 防吸血、封禁名单、事件日志 | 设置 → BitTorrent(标签页) |
| 关联 .torrent 文件 | 设置 → 通用 |

## 常见问题与限制

**Magnet 链接卡在"准备中"不动。** Magnet 链接本身不携带文件列表或大小信息——FluxDown 必须先从 DHT 和/或对等节点解析出这些元数据,这个过程在任务列表里显示为"准备中"。如果 5 分钟内没有任何节点响应,任务会失败并提示检查 Tracker 或网络(DHT 常被严格的防火墙或部分运营商屏蔽)。

**添加更多 Tracker 会不会丢掉内置的那些?** 社区订阅不会——它们会和你正在使用的列表合并。但手动编辑的自定义 Tracker 列表会**替换**内置的 25 个,而不是追加;如果想保留部分内置 Tracker,请把它们也写进你的列表。

**种子下载完后 FluxDown 会继续做种吗?** 会。下载完成的种子默认继续做种,并出现在下载列表的**做种中**筛选下;分享率、做种时长、最大同时做种任务数等限制在*设置 → BitTorrent*中。若想保护上传带宽不被只下不传的对端占用,见 [BT 防吸血](/docs/zh/protocols/bt-guard/)。
