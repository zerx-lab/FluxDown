---
title: BitTorrent
description: Magnet links and .torrent files with DHT, UPnP, built-in trackers and file selection.
section: protocols
order: 2
---

FluxDown has a BitTorrent engine built in (powered by librqbit) — you don't need a separate torrent client. It supports both magnet links and `.torrent` files, with DHT and UPnP enabled by default, a curated list of public trackers, and per-file selection before a multi-file torrent starts downloading.

## Adding a torrent

- **Magnet links** — paste a `magnet:` link into the New Download dialog exactly like an HTTP URL.
- **.torrent files** — open one from the New Download dialog's file picker, or associate `.torrent` files with FluxDown in *Settings → General → Associate .torrent Files*. Once associated, double-clicking a `.torrent` file anywhere on your system opens FluxDown and starts the download directly. (Windows only; this writes a per-user registry association and doesn't require administrator rights.)

<!-- TODO(screenshot): Settings → General, "Associate .torrent Files" toggle -->

## Selecting files

Multi-file torrents show a checklist of every file inside, with size, before the download starts — uncheck anything you don't want and FluxDown only fetches the files you selected.

<!-- TODO(screenshot): New Download dialog file-selection list for a multi-file torrent -->

## DHT, UPnP and listening port

Peer discovery works out of the box: **DHT** (trackerless peer discovery) and **UPnP port mapping** (automatic router port forwarding) are always enabled by the engine — there's nothing to configure. The only connectivity option exposed in *Settings → BitTorrent* is the **Listen Port Range** used for incoming BT connections (default **6881–6891**).

All BitTorrent tasks share one underlying session — one DHT routing table, one set of tracker connections, one listening port — rather than each task opening its own, which keeps resource usage low even with several torrents running at once.

## Trackers

FluxDown ships with **25 built-in public trackers**, curated for good global coverage: CN/Asia trackers first (for better peer locality if you're in mainland China or nearby), then international ones, with a few HTTPS fallbacks for networks that block UDP. These are used automatically for every torrent — including magnet links that don't specify any trackers of their own — unless you override them.

- **Custom trackers** — in *Settings → BitTorrent → Tracker List*, paste your own list (one URL per line) to replace the built-in set.
- **Tracker subscription** — *Settings → BitTorrent → Tracker Subscription* periodically fetches community-maintained tracker lists (`trackerslist.com` and `ngosang/trackerslist` by default, refreshed every 24 hours) and merges them in, deduplicated, alongside whatever tracker list you're using.

<!-- TODO(screenshot): Settings → BitTorrent, Tracker List and Tracker Subscription panels -->

## Resume support

Torrent downloads resume using BitTorrent's own piece-based verification: closing FluxDown, losing power, or a crash never loses already-verified pieces — only the piece that was in flight at the time needs to be re-fetched.

## Anti-leech protection

FluxDown watches peers for leech behavior — re-downloading the same data, faking progress, known leech clients — and can observe, throttle, stop uploading to, or block them. Every rule starts in **Observe only** mode, private torrents and local-network peers are exempt, and a layered block list plus a signed cloud list are available. See [BT Anti-Leech](/docs/en/protocols/bt-guard/) for details.

## Related settings

| Setting | Location |
|---|---|
| Listen Port Range | Settings → BitTorrent |
| Tracker List | Settings → BitTorrent |
| Tracker Subscription | Settings → BitTorrent |
| Anti-Leech, Block list, Event log | Settings → BitTorrent (tabs) |
| Associate .torrent Files | Settings → General |

## Limitations & FAQ

**A magnet link is stuck on "preparing".** Magnet links carry no file list or size up front — FluxDown has to resolve that metadata from DHT and/or peers first, which shows as "preparing" in the task list. If no peer or DHT node responds within 5 minutes, the task fails with an error telling you to check the trackers or your network (DHT is commonly blocked by restrictive firewalls or some ISPs).

**Can I add more trackers without losing the built-in ones?** Yes for community subscriptions — those merge with whatever list you're using. A hand-edited custom Tracker List, however, replaces the built-in 25 rather than adding to them; include the ones you want to keep.

**Does FluxDown seed after a torrent finishes?** Yes. Finished torrents keep seeding by default and appear under the **Seeding** filter in the download list; seeding limits (share ratio, seeding time, maximum active seeding tasks) are in *Settings → BitTorrent*. To protect your upload bandwidth from peers that only take, see [BT Anti-Leech](/docs/en/protocols/bt-guard/).
