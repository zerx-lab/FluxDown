//! librqbit [`PeerPolicy`] 适配器：把 rqbit 的连接前判定 / 逐事件回调一对一转换成 [`BtGuard`]
//! 的 [`GuardCtx`] / [`GuardEvent`] / [`Verdict`]。
//!
//! 唯一依赖 librqbit 的 bt_guard 模块；其余逻辑保持与 BT 后端解耦。
//!
//! 热路径（每个 16 KiB 块都会调用 `on_event`）不做 IO、不分配：`GuardCtx` 与 `GuardEvent`
//! 全部是栈上拷贝 / 借用；只有整块位图（`Bitfield` / `Disconnected`，每连接各一次）会构造
//! 一份 [`PieceSet`]。

use std::net::IpAddr;
use std::sync::Arc;

use librqbit::{PeerCtx, PeerEvent, PeerPolicy, PeerVerdict};

use super::{BtGuard, GuardCtx, GuardEvent, PieceSet, Verdict};

/// 包装 `Arc<BtGuard>` 的 [`PeerPolicy`]。防吸血关闭时 `BtGuard` 自身快速返回 `Allow` / `true`。
pub struct GuardPolicy {
    guard: Arc<BtGuard>,
}

impl GuardPolicy {
    #[must_use]
    pub fn new(guard: Arc<BtGuard>) -> Self {
        Self { guard }
    }
}

/// rqbit 连接上下文 → guard 上下文（全部 `Copy` 字段，无分配）。
#[must_use]
pub fn guard_ctx(ctx: &PeerCtx<'_>) -> GuardCtx {
    GuardCtx {
        info_hash: ctx.info_hash.0,
        addr: ctx.addr,
        private: ctx.private,
        total_length: ctx.total_length,
        piece_length: ctx.piece_length,
        num_pieces: ctx.num_pieces,
        we_are_seeding: ctx.we_are_seeding,
    }
}

/// guard 判决 → rqbit 判决（严重度一一对应）。
#[must_use]
pub fn peer_verdict(verdict: Verdict) -> PeerVerdict {
    match verdict {
        Verdict::Allow => PeerVerdict::Allow,
        Verdict::Throttle => PeerVerdict::Throttle,
        Verdict::Choke => PeerVerdict::Choke,
        Verdict::Disconnect => PeerVerdict::Disconnect,
    }
}

impl PeerPolicy for GuardPolicy {
    fn admit(&self, ip: IpAddr) -> bool {
        self.guard.admit(ip)
    }

    /// 重新询问被拒的出站对端（解封 / 到期释放、会话任务定时重试）：只看当前是否仍被封禁，
    /// 不计入「拦截连接数」——真正的连接尝试才算一次拦截。
    fn readmit(&self, ip: IpAddr) -> bool {
        !self.guard.is_banned(ip)
    }

    fn on_event(&self, ctx: &PeerCtx<'_>, event: PeerEvent<'_>) -> PeerVerdict {
        let ctx = guard_ctx(ctx);
        let verdict = match event {
            PeerEvent::Handshake {
                peer_id,
                reserved,
                incoming,
            } => self.guard.on_event(
                &ctx,
                GuardEvent::Handshake {
                    peer_id: peer_id.0,
                    reserved,
                    incoming,
                },
            ),
            PeerEvent::ExtendedHandshake { client } => self
                .guard
                .on_event(&ctx, GuardEvent::ExtendedHandshake { client }),
            PeerEvent::Bitfield { bits } => {
                let set = PieceSet::from_indices(bits.len(), bits.iter_ones());
                self.guard
                    .on_event(&ctx, GuardEvent::Bitfield { bits: &set })
            }
            PeerEvent::Have { piece } => self.guard.on_event(&ctx, GuardEvent::Have { piece }),
            PeerEvent::Request {
                piece,
                begin,
                length,
                peer_claims_piece,
            } => self.guard.on_event(
                &ctx,
                GuardEvent::Request {
                    piece,
                    begin,
                    length,
                    peer_claims_piece,
                },
            ),
            PeerEvent::Cancel {
                piece,
                begin,
                length,
            } => self.guard.on_event(
                &ctx,
                GuardEvent::Cancel {
                    piece,
                    begin,
                    length,
                },
            ),
            PeerEvent::Uploaded {
                piece,
                begin,
                length,
            } => self.guard.on_event(
                &ctx,
                GuardEvent::Uploaded {
                    piece,
                    begin,
                    length,
                },
            ),
            PeerEvent::Disconnected { bits } => {
                let set = bits.map(|bits| PieceSet::from_indices(bits.len(), bits.iter_ones()));
                self.guard
                    .on_event(&ctx, GuardEvent::Disconnected { bits: set.as_ref() })
            }
        };
        peer_verdict(verdict)
    }
}
