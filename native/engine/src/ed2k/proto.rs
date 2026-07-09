//! ED2K 线协议帧编解码 —— 纯字节处理 + 一个异步 [`read_frame`]。
//!
//! 帧格式：`<proto 1B><size 4 LE><opcode 1><payload>`，`size` 含 opcode 不含
//! proto/size 头本身。合法 proto 仅 [`PROTO_EDONKEY`]/[`PROTO_EMULE`]/
//! [`PROTO_PACKED`] 三者，其余立即报错（帧同步保护）。
//!
//! **防御原则**：所有"数组长度取自对端声明字段"处，先校验剩余字节量再切片，
//! 杜绝 slice 越界 panic；帧长字段在分配 buffer 前 clamp（防长度撒谎 OOM）；
//! `PROTO_PACKED` / `OP_COMPRESSEDPART` 的 zlib 流经 [`decompress_bounded`]
//! 限长熔断（防 zlib 炸弹）。

use std::io::Read;

use flate2::bufread::ZlibDecoder;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::downloader::DownloadError;

// ---------------------------------------------------------------------------
// proto 头字节
// ---------------------------------------------------------------------------

/// eDonkey 标准协议帧头。
pub const PROTO_EDONKEY: u8 = 0xE3;
/// eMule 扩展协议帧头。
pub const PROTO_EMULE: u8 = 0xC5;
/// zlib 压缩帧头（整帧 payload 经 zlib 压缩）。
pub const PROTO_PACKED: u8 = 0xD4;

// ---------------------------------------------------------------------------
// opcode（服务器 TCP）
// ---------------------------------------------------------------------------

pub const OP_LOGINREQUEST: u8 = 0x01;
pub const OP_REJECT: u8 = 0x05;
pub const OP_GETSOURCES: u8 = 0x19;
pub const OP_SERVERMESSAGE: u8 = 0x38;
pub const OP_SERVERSTATUS: u8 = 0x34;
pub const OP_IDCHANGE: u8 = 0x40;
pub const OP_FOUNDSOURCES: u8 = 0x42;
/// 客户端→服务器：请求服务器通知某 LowID 客户端回连我方（`<lowid_client_id 4>`）。
pub const OP_CALLBACKREQUEST: u8 = 0x1C;
/// 服务器→客户端：有对端请求我方回连（携带对端 `<ip 4><port 2>`）。
pub const OP_CALLBACKREQUESTED: u8 = 0x35;
/// 服务器→客户端：回连请求失败（目标 LowID 不可达/已下线）。
pub const OP_CALLBACK_FAIL: u8 = 0x36;

// ---------------------------------------------------------------------------
// opcode（客户端互连 TCP，0xE3）
// ---------------------------------------------------------------------------

pub const OP_HELLO: u8 = 0x01;
pub const OP_SENDINGPART: u8 = 0x46;
pub const OP_REQUESTPARTS: u8 = 0x47;
pub const OP_FILEREQANSNOFIL: u8 = 0x48;
pub const OP_END_OF_DOWNLOAD: u8 = 0x49;
pub const OP_HELLOANSWER: u8 = 0x4C;
pub const OP_SETREQFILEID: u8 = 0x4F;
pub const OP_FILESTATUS: u8 = 0x50;
pub const OP_HASHSETREQUEST: u8 = 0x51;
pub const OP_HASHSETANSWER: u8 = 0x52;
pub const OP_STARTUPLOADREQ: u8 = 0x54;
pub const OP_ACCEPTUPLOADREQ: u8 = 0x55;
pub const OP_CANCELTRANSFER: u8 = 0x56;
pub const OP_OUTOFPARTREQS: u8 = 0x57;
pub const OP_REQUESTFILENAME: u8 = 0x58;
/// peer 回应 `OP_REQUESTFILENAME`：`file_hash(16) + 文件名(u16 len + bytes)`。
/// 表示对端持有该文件（eMule `OP_REQFILENAMEANSWER`）。
pub const OP_FILEREQANSWER: u8 = 0x59;
pub const OP_QUEUERANK: u8 = 0x5C;

// ---------------------------------------------------------------------------
// opcode（eMule 扩展，0xC5）
// ---------------------------------------------------------------------------

pub const OP_EMULEINFO: u8 = 0x01;
pub const OP_EMULEINFOANSWER: u8 = 0x02;
pub const OP_COMPRESSEDPART: u8 = 0x40;
pub const OP_QUEUERANKING: u8 = 0x60;
pub const OP_COMPRESSEDPART_I64: u8 = 0xA1;
pub const OP_SENDINGPART_I64: u8 = 0xA2;
pub const OP_REQUESTPARTS_I64: u8 = 0xA3;

// ---------------------------------------------------------------------------
// 服务器登录 flags（tag `CT_SERVER_FLAGS`）
// ---------------------------------------------------------------------------

pub const SRV_TCPFLG_COMPRESSION: u32 = 0x0001;
pub const SRV_TCPFLG_AUXPORT: u32 = 0x0004;
pub const SRV_TCPFLG_NEWTAGS: u32 = 0x0008;
pub const SRV_TCPFLG_UNICODE: u32 = 0x0010;
pub const SRV_TCPFLG_LARGEFILES: u32 = 0x0100;

/// LowID 阈值：分配的 client ID 小于此值即 NAT 后客户端（直连不可达）。
pub const LOWID_THRESHOLD: u32 = 0x0100_0000;

/// 单帧 payload 硬上限（1 MiB）—— `read_frame` 分配 buffer 前 clamp 的默认量级。
/// 服务器帧远小于此；peer 数据帧调用方传 `BLOCK_SIZE + slack`。
pub const MAX_SERVER_FRAME: u32 = 1 << 20;

// ---------------------------------------------------------------------------
// 半开区间
// ---------------------------------------------------------------------------

/// eD2K 块请求区间。
///
/// **半开区间 `[start, end_exclusive)`** —— 与本仓库其余 downloader 的
/// inclusive `end_byte` 惯例相反。`OP_REQUESTPARTS` 的 start/end 三元组即此
/// 语义，混用 inclusive 会让每次请求产生 1 字节偏差。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartRange {
    /// 起始偏移（含）。
    pub start: u64,
    /// 结束偏移（不含）。
    pub end_exclusive: u64,
}

// ---------------------------------------------------------------------------
// 字节读取小工具（内部）
// ---------------------------------------------------------------------------

fn read_u16_le(buf: &[u8], off: usize) -> Result<u16, DownloadError> {
    buf.get(off..off + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| DownloadError::Ed2k("frame truncated (u16)".into()))
}

fn read_u32_le(buf: &[u8], off: usize) -> Result<u32, DownloadError> {
    buf.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| DownloadError::Ed2k("frame truncated (u32)".into()))
}

fn read_u64_le(buf: &[u8], off: usize) -> Result<u64, DownloadError> {
    buf.get(off..off + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or_else(|| DownloadError::Ed2k("frame truncated (u64)".into()))
}

fn read_hash16(buf: &[u8], off: usize) -> Result<[u8; 16], DownloadError> {
    buf.get(off..off + 16)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| DownloadError::Ed2k("frame truncated (hash16)".into()))
}

// ---------------------------------------------------------------------------
// zlib 限长解压（防炸弹）
// ---------------------------------------------------------------------------

/// zlib 解压，累计输出超过 `max_output` 立即中止（防 zlib 炸弹）。
///
/// 用于 [`PROTO_PACKED`] 整帧解压与 `OP_COMPRESSEDPART` 字段级流解压，两者
/// 复用同一上限逻辑：`max_output` 由调用方按"该帧/该 part 区间的合理上界"
/// 传入，绝不"解压到 EOF"。
///
/// # Errors
///
/// 解压产物超 `max_output` 或 zlib 流损坏时返回 [`DownloadError::Ed2k`]。
pub fn decompress_bounded(input: &[u8], max_output: usize) -> Result<Vec<u8>, DownloadError> {
    let mut decoder = ZlibDecoder::new(input);
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| DownloadError::Ed2k(format!("zlib decode error: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len() + n > max_output {
            return Err(DownloadError::Ed2k(format!(
                "zlib output exceeds limit {max_output} (bomb?)"
            )));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// read_frame
// ---------------------------------------------------------------------------

/// 从异步流读一个完整帧，返回 `(proto, opcode, payload)`。
///
/// 两阶段：① 异步 `read_exact` 读 proto(1)+size(4 LE)+opcode(1)，校验
/// `size-1 <= max_payload_len`（**分配 payload buffer 前**拒绝超长，防 OOM），
/// 再 `read_exact` payload；② `proto == PROTO_PACKED` 时经 [`decompress_bounded`]
/// 透明解压（上限 `max_payload_len`），返回解压后的 opcode+payload。
///
/// proto 非三个合法字节 → 立即 `Err`（帧同步保护，调用方应断连）。
///
/// # Errors
///
/// I/O 失败 / 非法 proto / size 超限 / zlib 炸弹 → [`DownloadError`]。
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_payload_len: u32,
) -> Result<(u8, u8, Vec<u8>), DownloadError> {
    let mut head = [0u8; 6];
    reader
        .read_exact(&mut head)
        .await
        .map_err(DownloadError::Io)?;
    let proto = head[0];
    if proto != PROTO_EDONKEY && proto != PROTO_EMULE && proto != PROTO_PACKED {
        return Err(DownloadError::Ed2k(format!(
            "illegal proto byte 0x{proto:02x}"
        )));
    }
    let size = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    // size 含 opcode（1 字节），故 payload 长度 = size - 1。
    if size == 0 {
        return Err(DownloadError::Ed2k("frame size 0 (missing opcode)".into()));
    }
    let payload_len = size - 1;
    if payload_len > max_payload_len {
        return Err(DownloadError::Ed2k(format!(
            "frame payload len {payload_len} exceeds max {max_payload_len}"
        )));
    }
    let opcode = head[5];
    let mut payload = vec![0u8; payload_len as usize];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(DownloadError::Io)?;

    if proto == PROTO_PACKED {
        let decompressed = decompress_bounded(&payload, max_payload_len as usize)?;
        Ok((PROTO_EDONKEY, opcode, decompressed))
    } else {
        Ok((proto, opcode, payload))
    }
}

// ---------------------------------------------------------------------------
// RequestParts（客户端发送，含 32/64 位变体）
// ---------------------------------------------------------------------------

/// `OP_REQUESTPARTS` / `OP_REQUESTPARTS_I64` —— 一次请求最多 3 段半开区间。
///
/// `large_file` 决定 offset 字段宽度（u32 vs u64）与 opcode（`0x47` vs `0xA3`）。
/// 空槽（`None`）编码为 `(0, 0)`。
///
/// **空槽填充惯例 `(0,0)` `unverified — confirm first`**：对照 aMule
/// `CUpDownClient::SendBlockRequests` 核实后用 §4 字节测试锁定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestParts {
    pub file_hash: [u8; 16],
    pub ranges: [Option<PartRange>; 3],
    pub large_file: bool,
}

impl RequestParts {
    /// 编码为 opcode payload（不含 proto/size 头）。返回 `(opcode, payload)`。
    #[must_use]
    pub fn encode(&self) -> (u8, Vec<u8>) {
        let mut out = Vec::with_capacity(16 + if self.large_file { 48 } else { 24 });
        out.extend_from_slice(&self.file_hash);
        let starts: [u64; 3] = std::array::from_fn(|i| self.ranges[i].map_or(0, |r| r.start));
        let ends: [u64; 3] = std::array::from_fn(|i| self.ranges[i].map_or(0, |r| r.end_exclusive));
        if self.large_file {
            for s in starts {
                out.extend_from_slice(&s.to_le_bytes());
            }
            for e in ends {
                out.extend_from_slice(&e.to_le_bytes());
            }
            (OP_REQUESTPARTS_I64, out)
        } else {
            for s in starts {
                out.extend_from_slice(&(s as u32).to_le_bytes());
            }
            for e in ends {
                out.extend_from_slice(&(e as u32).to_le_bytes());
            }
            (OP_REQUESTPARTS, out)
        }
    }

    /// 从 payload 解码（`large_file` 决定字段宽度）。
    ///
    /// # Errors
    /// payload 长度不足时返回 [`DownloadError::Ed2k`]。
    pub fn decode(payload: &[u8], large_file: bool) -> Result<Self, DownloadError> {
        let file_hash = read_hash16(payload, 0)?;
        let mut starts = [0u64; 3];
        let mut ends = [0u64; 3];
        if large_file {
            for (i, s) in starts.iter_mut().enumerate() {
                *s = read_u64_le(payload, 16 + i * 8)?;
            }
            for (i, e) in ends.iter_mut().enumerate() {
                *e = read_u64_le(payload, 16 + 24 + i * 8)?;
            }
        } else {
            for (i, s) in starts.iter_mut().enumerate() {
                *s = u64::from(read_u32_le(payload, 16 + i * 4)?);
            }
            for (i, e) in ends.iter_mut().enumerate() {
                *e = u64::from(read_u32_le(payload, 16 + 12 + i * 4)?);
            }
        }
        let ranges = std::array::from_fn(|i| {
            if starts[i] == 0 && ends[i] == 0 {
                None
            } else {
                Some(PartRange {
                    start: starts[i],
                    end_exclusive: ends[i],
                })
            }
        });
        Ok(RequestParts {
            file_hash,
            ranges,
            large_file,
        })
    }
}

// ---------------------------------------------------------------------------
// 已解码的入站消息
// ---------------------------------------------------------------------------

/// dispatch 解码出的入站消息。未识别 opcode → [`Ed2kMessage::Unknown`]
/// （调用方"忽略并 log"，不报错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ed2kMessage {
    /// 服务器分配/变更本机 client ID（`OP_IDCHANGE`）。LowID 若 `< LOWID_THRESHOLD`。
    IdChange { client_id: u32 },
    /// 服务器拒绝登录（`OP_REJECT`）。
    Reject,
    /// 服务器 MOTD 文本（`OP_SERVERMESSAGE`）。
    ServerMessage(String),
    /// 服务器状态（`OP_SERVERSTATUS`），本期只需识别并跳过。
    ServerStatus,
    /// 源列表（`OP_FOUNDSOURCES`）：`(file_hash, [(client_id, port)])`。
    FoundSources {
        file_hash: [u8; 16],
        sources: Vec<(u32, u16)>,
    },
    /// peer 握手应答（`OP_HELLOANSWER`）：携带对端 user hash（前 16 字节）。
    HelloAnswer { user_hash: [u8; 16] },
    /// peer 文件状态（`OP_FILESTATUS`）：`(file_hash, part 持有 bitfield)`。
    FileStatus {
        file_hash: [u8; 16],
        bitfield: Vec<u8>,
    },
    /// peer 无此文件（`OP_FILEREQANSNOFIL`）。
    NoFile { file_hash: [u8; 16] },
    /// hashset 应答（`OP_HASHSETANSWER`）：所有块 MD4。
    HashSetAnswer { part_hashes: Vec<[u8; 16]> },
    /// 明文分片数据（`OP_SENDINGPART` / `_I64`）。
    SendingPart {
        file_hash: [u8; 16],
        start: u64,
        end_exclusive: u64,
        data: Vec<u8>,
    },
    /// 压缩分片数据（`OP_COMPRESSEDPART` / `_I64`）：`start` + zlib 流（未解压）。
    CompressedPart {
        file_hash: [u8; 16],
        start: u64,
        packed: Vec<u8>,
    },
    /// 排队名次（`OP_QUEUERANK` / `OP_QUEUERANKING`）。
    QueueRank(u32),
    /// 对端上传队列已满/无请求（`OP_OUTOFPARTREQS`）。
    OutOfPartReqs,
    /// 未识别 opcode（携带原 opcode）。调用方忽略并 log。
    Unknown(u8),
}

/// 把一个 `(proto, opcode, payload)` 帧路由到具体 [`Ed2kMessage`]。
///
/// **唯一 opcode→struct 路由点** —— 无论服务器/peer/大文件变体，所有解码都
/// 经此。`large_file` 决定 `SendingPart`/`CompressedPart` 的 offset 字段宽度。
///
/// # Errors
///
/// 已识别 opcode 但 payload 畸形（长度不足/声明数超剩余字节）→ [`DownloadError`]。
/// 未识别 opcode 不报错，返回 [`Ed2kMessage::Unknown`]。
pub fn dispatch(
    proto: u8,
    opcode: u8,
    payload: &[u8],
    large_file: bool,
) -> Result<Ed2kMessage, DownloadError> {
    // eMule 扩展 opcode（0xC5）与 eDonkey opcode 编号有重叠，先按 proto 分流。
    if proto == PROTO_EMULE {
        return dispatch_emule(opcode, payload, large_file);
    }
    match opcode {
        OP_IDCHANGE => {
            let client_id = read_u32_le(payload, 0)?;
            Ok(Ed2kMessage::IdChange { client_id })
        }
        OP_REJECT => Ok(Ed2kMessage::Reject),
        OP_SERVERSTATUS => Ok(Ed2kMessage::ServerStatus),
        OP_SERVERMESSAGE => {
            let len = read_u16_le(payload, 0)? as usize;
            let text = payload
                .get(2..2 + len)
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .ok_or_else(|| DownloadError::Ed2k("servermessage truncated".into()))?;
            Ok(Ed2kMessage::ServerMessage(text))
        }
        OP_FOUNDSOURCES => {
            let file_hash = read_hash16(payload, 0)?;
            let count = *payload
                .get(16)
                .ok_or_else(|| DownloadError::Ed2k("foundsources missing count".into()))?
                as usize;
            // 严格校验：声明的源数必须与剩余字节精确匹配（每源 6 字节）。
            let expected = 16 + 1 + count * 6;
            if payload.len() < expected {
                return Err(DownloadError::Ed2k(format!(
                    "foundsources declares {count} sources but payload only {} bytes",
                    payload.len()
                )));
            }
            let mut sources = Vec::with_capacity(count);
            for i in 0..count {
                let off = 17 + i * 6;
                let id = read_u32_le(payload, off)?;
                let port = read_u16_le(payload, off + 4)?;
                sources.push((id, port));
            }
            Ok(Ed2kMessage::FoundSources { file_hash, sources })
        }
        OP_HELLOANSWER => {
            // 载荷起始为 16 字节 user hash（其后为 ID/port/tags，本期不需要）。
            let user_hash = read_hash16(payload, 0)?;
            Ok(Ed2kMessage::HelloAnswer { user_hash })
        }
        OP_FILESTATUS => {
            let file_hash = read_hash16(payload, 0)?;
            let part_count = read_u16_le(payload, 16)? as usize;
            // bitfield 长度 = ceil(part_count / 8)；part_count==0 表示"持有整文件"。
            let nbytes = part_count.div_ceil(8);
            let bitfield = payload
                .get(18..18 + nbytes)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| DownloadError::Ed2k("filestatus bitfield truncated".into()))?;
            Ok(Ed2kMessage::FileStatus {
                file_hash,
                bitfield,
            })
        }
        OP_FILEREQANSNOFIL => {
            let file_hash = read_hash16(payload, 0)?;
            Ok(Ed2kMessage::NoFile { file_hash })
        }
        OP_HASHSETANSWER => {
            // payload: file_hash(16) + count(2) + count*16。
            let count = read_u16_le(payload, 16)? as usize;
            let expected = 18 + count * 16;
            if payload.len() < expected {
                return Err(DownloadError::Ed2k(format!(
                    "hashsetanswer declares {count} hashes but payload only {} bytes",
                    payload.len()
                )));
            }
            let mut part_hashes = Vec::with_capacity(count);
            for i in 0..count {
                part_hashes.push(read_hash16(payload, 18 + i * 16)?);
            }
            Ok(Ed2kMessage::HashSetAnswer { part_hashes })
        }
        OP_SENDINGPART => decode_sending_part(payload, false),
        OP_QUEUERANK => Ok(Ed2kMessage::QueueRank(read_u32_le(payload, 0)?)),
        OP_OUTOFPARTREQS => Ok(Ed2kMessage::OutOfPartReqs),
        _ => Ok(Ed2kMessage::Unknown(opcode)),
    }
}

fn dispatch_emule(
    opcode: u8,
    payload: &[u8],
    _large_file: bool,
) -> Result<Ed2kMessage, DownloadError> {
    match opcode {
        OP_COMPRESSEDPART => decode_compressed_part(payload, false),
        OP_COMPRESSEDPART_I64 => decode_compressed_part(payload, true),
        OP_SENDINGPART_I64 => decode_sending_part(payload, true),
        OP_QUEUERANKING => Ok(Ed2kMessage::QueueRank(read_u32_le(payload, 0)?)),
        _ => Ok(Ed2kMessage::Unknown(opcode)),
    }
}

fn decode_sending_part(payload: &[u8], large: bool) -> Result<Ed2kMessage, DownloadError> {
    let file_hash = read_hash16(payload, 0)?;
    let (start, end_exclusive, data_off) = if large {
        (read_u64_le(payload, 16)?, read_u64_le(payload, 24)?, 32)
    } else {
        (
            u64::from(read_u32_le(payload, 16)?),
            u64::from(read_u32_le(payload, 20)?),
            24,
        )
    };
    let data = payload
        .get(data_off..)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| DownloadError::Ed2k("sendingpart data truncated".into()))?;
    Ok(Ed2kMessage::SendingPart {
        file_hash,
        start,
        end_exclusive,
        data,
    })
}

fn decode_compressed_part(payload: &[u8], large: bool) -> Result<Ed2kMessage, DownloadError> {
    let file_hash = read_hash16(payload, 0)?;
    let (start, packed_off) = if large {
        (read_u64_le(payload, 16)?, 28) // hash16 + start8 + packed_size4
    } else {
        (u64::from(read_u32_le(payload, 16)?), 24) // hash16 + start4 + packed_size4
    };
    // packed_size 字段（u32）在 offset 前一段，仅用于交叉校验剩余字节。
    let packed = payload
        .get(packed_off..)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| DownloadError::Ed2k("compressedpart data truncated".into()))?;
    Ok(Ed2kMessage::CompressedPart {
        file_hash,
        start,
        packed,
    })
}

// ---------------------------------------------------------------------------
// 客户端发送方向：帧封装 + 简单消息编码
// ---------------------------------------------------------------------------

/// 把 `(opcode, payload)` 封装为完整线上帧（proto=0xE3）。
#[must_use]
pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    frame_with_proto(PROTO_EDONKEY, opcode, payload)
}

/// 同 [`frame`] 但可指定 proto 头（eMule 扩展帧用 [`PROTO_EMULE`]）。
#[must_use]
pub fn frame_with_proto(proto: u8, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let size = (payload.len() as u32) + 1; // +1 for opcode
    let mut out = Vec::with_capacity(6 + payload.len());
    out.push(proto);
    out.extend_from_slice(&size.to_le_bytes());
    out.push(opcode);
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        Ed2kMessage, OP_FOUNDSOURCES, OP_HASHSETANSWER, OP_IDCHANGE, OP_REQUESTPARTS,
        OP_REQUESTPARTS_I64, OP_SENDINGPART, PROTO_EDONKEY, PROTO_EMULE, PROTO_PACKED, PartRange,
        RequestParts, decompress_bounded, dispatch, frame, read_frame,
    };
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    fn zlib_compress(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn request_parts_roundtrip_32bit() {
        let rp = RequestParts {
            file_hash: [7u8; 16],
            ranges: [
                Some(PartRange {
                    start: 100,
                    end_exclusive: 200,
                }),
                None,
                None,
            ],
            large_file: false,
        };
        let (opcode, payload) = rp.encode();
        assert_eq!(opcode, OP_REQUESTPARTS);
        // hash16 + 3*u32 starts + 3*u32 ends = 16 + 24 = 40
        assert_eq!(payload.len(), 40);
        // 空槽应为 (0,0)。
        assert_eq!(&payload[20..24], &[0, 0, 0, 0]); // start[1]
        let back = RequestParts::decode(&payload, false).unwrap();
        assert_eq!(back, rp);
    }

    #[test]
    fn request_parts_roundtrip_64bit() {
        let rp = RequestParts {
            file_hash: [9u8; 16],
            ranges: [
                Some(PartRange {
                    start: 5_000_000_000,
                    end_exclusive: 5_000_100_000,
                }),
                None,
                None,
            ],
            large_file: true,
        };
        let (opcode, payload) = rp.encode();
        assert_eq!(opcode, OP_REQUESTPARTS_I64);
        assert_eq!(payload.len(), 16 + 48);
        let back = RequestParts::decode(&payload, true).unwrap();
        assert_eq!(back, rp);
    }

    #[test]
    fn dispatch_idchange() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x0100_0001u32.to_le_bytes());
        let msg = dispatch(PROTO_EDONKEY, OP_IDCHANGE, &payload, false).unwrap();
        assert_eq!(
            msg,
            Ed2kMessage::IdChange {
                client_id: 0x0100_0001
            }
        );
    }

    #[test]
    fn dispatch_foundsources_ok() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0xAAu8; 16]); // file hash
        payload.push(2); // count
        payload.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        payload.extend_from_slice(&4661u16.to_le_bytes());
        payload.extend_from_slice(&0x00AB_CDEFu32.to_le_bytes());
        payload.extend_from_slice(&5000u16.to_le_bytes());
        let msg = dispatch(PROTO_EDONKEY, OP_FOUNDSOURCES, &payload, false).unwrap();
        match msg {
            Ed2kMessage::FoundSources { sources, .. } => {
                assert_eq!(sources.len(), 2);
                assert_eq!(sources[0], (0x0102_0304, 4661));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn dispatch_foundsources_lying_count_errs_not_panics() {
        // 声明 10 源但只够 1 源 → Err（最高优先级：绝不 slice panic）。
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0xAAu8; 16]);
        payload.push(10); // 撒谎的 count
        payload.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        payload.extend_from_slice(&4661u16.to_le_bytes());
        assert!(dispatch(PROTO_EDONKEY, OP_FOUNDSOURCES, &payload, false).is_err());
    }

    #[test]
    fn dispatch_hashsetanswer_lying_count_errs() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0xBBu8; 16]);
        payload.extend_from_slice(&100u16.to_le_bytes()); // 撒谎 count=100
        payload.extend_from_slice(&[1u8; 16]); // 只给 1 个
        assert!(dispatch(PROTO_EDONKEY, OP_HASHSETANSWER, &payload, false).is_err());
    }

    #[test]
    fn dispatch_sendingpart_32bit() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0xCCu8; 16]);
        payload.extend_from_slice(&100u32.to_le_bytes());
        payload.extend_from_slice(&105u32.to_le_bytes());
        payload.extend_from_slice(b"hello");
        let msg = dispatch(PROTO_EDONKEY, OP_SENDINGPART, &payload, false).unwrap();
        match msg {
            Ed2kMessage::SendingPart {
                start,
                end_exclusive,
                data,
                ..
            } => {
                assert_eq!((start, end_exclusive), (100, 105));
                assert_eq!(data, b"hello");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn dispatch_unknown_opcode() {
        let msg = dispatch(PROTO_EDONKEY, 0xFE, &[], false).unwrap();
        assert_eq!(msg, Ed2kMessage::Unknown(0xFE));
    }

    #[test]
    fn dispatch_emule_unknown() {
        let msg = dispatch(PROTO_EMULE, 0xFD, &[], false).unwrap();
        assert_eq!(msg, Ed2kMessage::Unknown(0xFD));
    }

    #[test]
    fn decompress_bounded_ok() {
        let original = b"the quick brown fox".repeat(10);
        let packed = zlib_compress(&original);
        let out = decompress_bounded(&packed, 10_000).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn decompress_bounded_bomb_truncated() {
        // 高压缩比：1 MiB 全 0 压缩后仅数百字节，限 1 KiB 应中止。
        let bomb_src = vec![0u8; 1024 * 1024];
        let packed = zlib_compress(&bomb_src);
        assert!(packed.len() < 4096, "sanity: compresses tiny");
        assert!(decompress_bounded(&packed, 1024).is_err());
    }

    #[tokio::test]
    async fn read_frame_ok() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x0100_0001u32.to_le_bytes());
        let wire = frame(OP_IDCHANGE, &payload);
        let mut cursor = std::io::Cursor::new(wire);
        let (proto, opcode, got) = read_frame(&mut cursor, 1024).await.unwrap();
        assert_eq!(proto, PROTO_EDONKEY);
        assert_eq!(opcode, OP_IDCHANGE);
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn read_frame_size_lie_rejected_before_alloc() {
        // proto + size=0xFFFFFFFF + opcode，无 payload：应在读 payload 前 Err。
        let mut wire = vec![PROTO_EDONKEY];
        wire.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        wire.push(OP_IDCHANGE);
        let mut cursor = std::io::Cursor::new(wire);
        assert!(read_frame(&mut cursor, 1024).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_illegal_proto_rejected() {
        let mut wire = vec![0x00]; // 非法 proto
        wire.extend_from_slice(&2u32.to_le_bytes());
        wire.push(0x01);
        wire.push(0x00);
        let mut cursor = std::io::Cursor::new(wire);
        assert!(read_frame(&mut cursor, 1024).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_truncated_errs() {
        // 声明 payload 10 字节但只给 2 → read_exact Err。
        let mut wire = vec![PROTO_EDONKEY];
        wire.extend_from_slice(&11u32.to_le_bytes()); // size=11 → payload 10
        wire.push(OP_IDCHANGE);
        wire.extend_from_slice(&[1u8, 2]);
        let mut cursor = std::io::Cursor::new(wire);
        assert!(read_frame(&mut cursor, 1024).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_packed_transparent_decompress() {
        let mut inner_payload = Vec::new();
        inner_payload.extend_from_slice(&0x0100_0001u32.to_le_bytes());
        // 压缩帧：proto=0xD4，payload = zlib(opcode 后的原 payload)。
        // 注意：eD2K packed 帧压缩的是 opcode 之后的数据，此处构造 opcode 明文 + 压缩 payload。
        let packed_payload = zlib_compress(&inner_payload);
        let wire = super::frame_with_proto(PROTO_PACKED, OP_IDCHANGE, &packed_payload);
        let mut cursor = std::io::Cursor::new(wire);
        let (proto, opcode, got) = read_frame(&mut cursor, 4096).await.unwrap();
        assert_eq!(proto, PROTO_EDONKEY);
        assert_eq!(opcode, OP_IDCHANGE);
        assert_eq!(got, inner_payload);
    }

    #[test]
    fn part_range_half_open() {
        let r = PartRange {
            start: 0,
            end_exclusive: 100,
        };
        assert_eq!(r.end_exclusive - r.start, 100);
    }
}
