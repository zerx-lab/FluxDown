---
title: BT Anti-Leech
description: Detect peers that only take from you or fake their progress, then observe, throttle, choke or block them — with layered block lists, a signed cloud list and opt-in evidence sharing.
section: protocols
order: 6
---

Some BitTorrent peers (known as "leech" or "vampire" clients) download from you again and again, or lie about their progress to keep you uploading. **BT Anti-Leech** is FluxDown's built-in defence: it watches how each peer behaves, records what a rule *would* do, and — only for the rules you switch on — slows, stops uploading to, or blocks that peer.

## Defaults

Out of the box FluxDown is deliberately cautious:

- Protection is **on**, but every detection rule and the cloud list are set to **Observe only**: hits are written to the event log, nothing is throttled or blocked. Review the log, then switch the rules you trust to **Enforce**.
- **Private torrents are skipped** (not inspected, never reported).
- **Local network peers are always exempt**: private ranges, loopback, link-local, carrier-grade NAT (100.64.0.0/10) and IPv6 ULA addresses.
- On a task that is **still downloading**, the strongest action is **Throttle** (configurable), so a wrong verdict cannot stall your own download.

## Architecture

![BT anti-leech architecture: peers, librqbit hooks, the engine bt_guard module, daemon RPC, agent, clients and FluxCloud](/docs/bt-guard/architecture.svg)

Peers connect to FluxDown's librqbit fork, which calls hooks for admitting a connection, reporting protocol events and scheduling uploads. The engine's `bt_guard` module judges those events and answers allow / throttle / choke / disconnect. Results flow through the daemon to the agent, which also fetches and verifies the cloud list, uploads evidence (if you opted in) and exposes the REST API. All four clients show the same state.

## Detection rules

Each rule has three modes: **Off**, **Observe only** and **Enforce**. A peer is identified by its *IP group*: a single address for IPv4, and a prefix (default `/64`, configurable from `/48` to `/128`) for IPv6.

| Rule | Strength | What it detects | Default |
|---|---|---|---|
| Excessive upload | Strong | A peer group has downloaded from you more than 200% of the torrent size (and at least 64 MiB — smaller torrents never trigger it). | Observe |
| Re-requesting owned pieces | Strong | The peer asks for pieces it has already announced it owns, for at least two different pieces. | Observe |
| Progress rewind | Strong | A peer reconnects reporting noticeably fewer pieces than before (more than 7% lost) so it can download the same data again. | Observe |
| Progress lag (probe) | Medium | Over half of the pieces you sent (after a 120-second grace) never show up in the peer's progress. FluxDown disconnects it once as a probe: if it comes back still missing them it counts as a rewind, if it catches up the suspicion is cleared. At most one probe per group per 24 hours. | Observe |
| Client fingerprint | Weak | The client name or peer ID matches a known leech client from the cloud list. Never bans on its own. | Observe |
| Cloud list | Per entry | The peer's address is on the signed cloud list; the entry decides ban, choke or throttle. | Observe |

## Actions

Actions only happen for rules in **Enforce** mode; in **Observe only** mode the action that *would* have happened is just recorded.

| Signal | Action |
|---|---|
| Strong | A strike, then a **ban**: 24 hours for the first strike, ×4 for each repeat (24 h → 4 d → 16 d …), capped at 30 days. Strikes reset after 30 days without a new one. |
| Medium or weak | Seeding task: **choke** (stop uploading). Downloading task: **throttle** (shared upload cap, 32 KiB/s by default). Never a ban on their own. |
| Cloud list entry | The entry's own action: ban until it expires, or choke / throttle. |

A ban refuses new connections from the group and disconnects it on every torrent. The first ban duration, the maximum duration, the throttled speed, the thresholds above and the "action limit while downloading" are all configurable (see [Settings](/docs/en/getting-started/settings/)).

## Block list layers

Entries live in five layers; when several match, the higher one wins:

1. **Allow list** — always allowed (highest priority).
2. **Manual** — entries you add.
3. **Automatic** — bans created by the rules above.
4. **Cloud** — the signed cloud list.
5. **Filter file** — plain-text IP lists you subscribe to (one address or CIDR per line; PeerGuardian text also works), refreshed every 24 hours.

Cloud and filter-file entries wider than IPv4 `/16` or IPv6 `/32` are discarded, so a bad list cannot block a whole region. Cloud entries cannot be deleted locally — add the address to the allow list instead.

## Cloud list

FluxDown fetches a list signed by FluxCloud (about every 6 hours while the BT engine runs, only if "Use the cloud blocklist" is on) and verifies it **locally with ed25519** before use. Sources:

- **BTN-Collected-Rules** by PBH-BTN, licensed [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) — <https://github.com/PBH-BTN/BTN-Collected-Rules>, mirrored and re-signed by FluxCloud.
- **FluxCloud consensus list** — peers independently reported by several qualified users, and approved by an administrator before publishing.

The client protects itself against a bad or hijacked list. It rejects the whole document if the signature is invalid, the version number goes backwards, it has expired, it covers well-known public addresses (built-in canary IPs), or it differs by more than 50% from the previous version (for lists of 1000+ entries). The previous list is kept in that case and the reason is shown in the cloud section. An expired list is paused ("stale") until a new one arrives.

## Evidence sharing (optional)

You can help build the consensus list by reporting peers that trigger rules. This is **off by default**, stored on this device only, and requires both a signed-in FluxDown account and an explicit consent dialog.

- **Sent:** the offending peer's IP group, the rule that fired, byte counts and ratios, the client name or peer ID prefix, and the torrent's size and infohash.
- **Never sent:** file names, paths, torrent names, or anything from **private torrents**.
- FluxCloud hashes the infohash with a server-side key on arrival and keeps no original.

See the [Privacy Policy](/privacy/#bt-guard) for the full statement.

## Where to find it

- **Desktop and Web:** *Settings → BitTorrent* has three tabs — **Anti-Leech** (rules, thresholds, actions, exemptions, cloud list, filter subscriptions, evidence sharing, today's stats), **Block list** (browse, search, add, release, allow-list, export) and **Event log**. The BT details *Connections* page adds columns for **IP group**, **uploaded to group**, **suspicion**, **rules** and **action**; the right-click menu can ban a peer group for 1 hour / 1 day / 7 days / permanently, throttle it for a day, allow-list it, or open its evidence.
- **Android:** *Settings → BitTorrent* includes the anti-leech settings, the block list (with manual add and export) and an event log (filter by kind and rule, paged, with details).
- **iOS:** *Settings → BitTorrent* includes the anti-leech settings and the block list (browse, release, allow-list); there is no event log, manual add or export.
- The per-connection columns and right-click actions are desktop / web features.

## REST API

With the management API enabled (see [API overview](/docs/en/api/overview/)), the following routes are available; the full schema is in the OpenAPI document served at `/api/v1/openapi.json`.

| Route | Purpose |
|---|---|
| `GET /api/v1/bt/guard/state` | Master switch, session state, today's stats, cloud list status, subscription status, per-layer counts. |
| `GET /api/v1/bt/guard/entries` | List entries. Filters: `layer`, `rule`, `query`, `containsIp` (only entries whose CIDR covers that IP, including wide cloud ranges), `offset`, `limit` (1–500). |
| `POST /api/v1/bt/guard/entries` | Add a manual or allow entry. |
| `DELETE /api/v1/bt/guard/entries/{id}` | Remove an entry (release a ban). Cloud-layer entries return 400. |
| `GET /api/v1/bt/guard/events` | Event log. Filters: `rule`, `kind`, `ipGroup` (an IP or a group such as `2001:db8::/64`), `offset`, `limit`. |

## If you were blocked by mistake

Other FluxDown users may have blocked your address through the cloud list. Open the [BT Anti-Leech self-check page](/bt-guard/) from the network you use: it shows whether your current IP is listed, why, and when the entry expires, and lets you file an appeal. Approved appeals are removed and allow-listed. It only answers for the address you are visiting from — arbitrary lookups are not supported.

## Compatibility fixes

Two general fixes ship with the integrated BT engine, independent of the rules:

- FluxDown **always sends HAVE messages**. Suppressing "redundant" HAVEs could make progress-checking tools such as PeerBanHelper wrongly think a peer was leeching and ban you.
- Upload requests that the remote peer **already cancelled** are dropped before reading from disk.
