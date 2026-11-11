//! eMule Kad（Kademlia DHT）线协议编解码 —— 纯字节处理，可离线全测。
//!
//! 帧格式：`<0xE4|0xE5><opcode 1B><payload>`。[`parse_datagram`] 同时接受
//! 明文 [`PROTO_KAD`] 与 zlib packed [`PROTO_KAD_PACKED`]（eMule 对较大回包
//! 如 `SEARCH_RES` 一律压缩）。字节布局以真实网络抓包实证为准，两处与参考
//! 实现 goed2k 相反/不同（goed2k 该两处有 bug，其 live 测试因此依赖注入
//! fallback peers）：
//! - **IP 字节序**：Kad 线上与 nodes.dat 的 IP 是主机序 u32 的小端编码
//!   （线上字节 `[d,c,b,a]` → IP `a.b.c.d`），见 [`Endpoint`]；
//! - **tag 名字**：真实 `SEARCH_RES` 用标准 ed2k tag（`type + name_len(u16)
//!   + name`），非 0x80 单字节 ID 约定，两者均兼容。
//!
//! **防御原则**：所有"数组长度取自对端声明字段"处，先按剩余字节量 clamp
//! 再逐条解析，任一条失败即停并保留已解出的前缀，杜绝越界 panic 与 OOM。

use std::net::Ipv4Addr;

use crate::ed2k::server::PeerAddr;

// ---------------------------------------------------------------------------
// 帧头 / opcode / 常量
// ---------------------------------------------------------------------------

/// Kad 明文协议帧头。
pub const PROTO_KAD: u8 = 0xE4;
/// Kad zlib 压缩帧头（本实现只读明文，收到即丢弃）。
pub const PROTO_KAD_PACKED: u8 = 0xE5;

pub const OP_BOOTSTRAP_REQ: u8 = 0x01;
pub const OP_BOOTSTRAP_RES: u8 = 0x09;
pub const OP_REQ: u8 = 0x21;
pub const OP_RES: u8 = 0x29;
pub const OP_HELLO_REQ: u8 = 0x11;
pub const OP_HELLO_RES: u8 = 0x19;
pub const OP_HELLO_RES_ACK: u8 = 0x22;
pub const OP_SEARCH_SRC_REQ: u8 = 0x34;
pub const OP_SEARCH_RES: u8 = 0x3B;
pub const OP_PING: u8 = 0x60;
pub const OP_PONG: u8 = 0x61;

/// KADEMLIA2 协议版本（Hello/联系点携带）。
pub const KADEMLIA_VERSION: u8 = 0x05;

/// `Req` 的搜索类型：FindNode（逼近目标 ID）。
pub const SEARCH_FIND_NODE: u8 = 0x0B;
/// `Req` 的搜索类型：FindValue。
pub const SEARCH_FIND_VALUE: u8 = 0x02;
/// `Req` 的搜索类型：Store。
pub const SEARCH_STORE: u8 = 0x04;

// Tag 值类型（低 7 位；高位 0x80 表示带 1 字节 ID）。
const TAG_TYPE_STRING: u8 = 0x02;
const TAG_TYPE_UINT32: u8 = 0x03;
const TAG_TYPE_UINT16: u8 = 0x08;
const TAG_TYPE_UINT8: u8 = 0x09;
const TAG_TYPE_UINT64: u8 = 0x0B;
const TAG_TYPE_STR1: u8 = 0x11;
const TAG_TYPE_STR16: u8 = 0x20;

// 源相关 Tag ID。
const TAG_SOURCE_TYPE: u8 = 0xFF;
const TAG_SOURCE_IP: u8 = 0xFE;
const TAG_SOURCE_PORT: u8 = 0xFD;

/// 单个 Kad 联系点线上大小：KadID(16)+IP(4)+UDP(2)+TCP(2)+Version(1)=25。
const CONTACT_WIRE_SIZE: usize = 25;

// ---------------------------------------------------------------------------
// 防御式字节读取器
// ---------------------------------------------------------------------------

/// 只前进、永不 panic 的字节读取器；越界一律返回 `None`。
struct ByteReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.remaining() < n {
            return None;
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    fn u16_le(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_le_bytes([s[0], s[1]]))
    }

    fn u32_le(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn u64_le(&mut self) -> Option<u64> {
        self.take(8)
            .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
    }

    fn array16(&mut self) -> Option<[u8; 16]> {
        let s = self.take(16)?;
        let mut out = [0u8; 16];
        out.copy_from_slice(s);
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// KadID（128bit）—— 线上是 4 个小端 u32，内存为大端字节序（dword 内反转）
// ---------------------------------------------------------------------------

/// Kademlia 节点/文件标识。内部 [`KadId::0`] 是"内存字节序"（可直接 XOR 比距）；
/// 线上编解码经每 4 字节 dword 内字节反转。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KadId(pub [u8; 16]);

/// 每 4 字节 dword 内字节反转：`out[(i/4)*4 + 3 - (i%4)] = src[i]`。
///
/// 该变换是对合（自身逆运算），故收发共用。
fn dword_reverse(src: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[(i / 4) * 4 + 3 - (i % 4)] = src[i];
    }
    out
}

impl KadId {
    /// 尚不知道 KadID 的节点（用户只给了地址）的占位值。真实 ID 是 128 位随机数，不会全零。
    pub const UNKNOWN: KadId = KadId([0; 16]);

    /// 从内存字节序（如 ed2k 文件 hash）直接构造，不做变换。
    pub fn from_memory(bytes: &[u8; 16]) -> Self {
        KadId(*bytes)
    }

    /// 解码线上 16 字节（dword 反转 → 内存序）。
    fn decode(reader: &mut ByteReader<'_>) -> Option<Self> {
        let wire = reader.array16()?;
        Some(KadId(dword_reverse(&wire)))
    }

    /// 编码为线上 16 字节（内存序 → dword 反转）。
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&dword_reverse(&self.0));
    }

    /// 与 `target` 的 XOR 距离比较：`self` 更近返回 `Less`。
    pub fn xor_cmp(&self, other: &KadId, target: &KadId) -> std::cmp::Ordering {
        for i in 0..16 {
            let da = self.0[i] ^ target.0[i];
            let db = other.0[i] ^ target.0[i];
            match da.cmp(&db) {
                std::cmp::Ordering::Equal => continue,
                ord => return ord,
            }
        }
        std::cmp::Ordering::Equal
    }
}

// ---------------------------------------------------------------------------
// Endpoint（8B）: IP u32 LE + UDPPort u16 LE + TCPPort u16 LE
// ---------------------------------------------------------------------------

/// Kad 联系点网络端点。IP 线上是 **host-order u32 的小端编码**（eMule
/// `CFileDataIO::WriteUInt32(GetIPAddress())`，`GetIPAddress()` 为主机序），
/// 即线上字节 `[d,c,b,a]` 对应 IP `a.b.c.d` —— 与 ed2k TCP 服务器协议的
/// HighID 直序（`[a,b,c,d]` → `a.b.c.d`）**相反**。经真实 nodes.dat 实证：
/// 直序解析使 37/192 个联系点落入 DoD/Apple/组播等不可能路由段，反序为 0。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub ip: Ipv4Addr,
    pub udp_port: u16,
    pub tcp_port: u16,
}

impl Endpoint {
    fn decode(reader: &mut ByteReader<'_>) -> Option<Self> {
        let b = reader.take(4)?;
        let ip = Ipv4Addr::new(b[3], b[2], b[1], b[0]);
        let udp_port = reader.u16_le()?;
        let tcp_port = reader.u16_le()?;
        Some(Endpoint {
            ip,
            udp_port,
            tcp_port,
        })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        let o = self.ip.octets();
        out.extend_from_slice(&[o[3], o[2], o[1], o[0]]);
        out.extend_from_slice(&self.udp_port.to_le_bytes());
        out.extend_from_slice(&self.tcp_port.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Contact / Entry（25B）: KadID + Endpoint + Version
// ---------------------------------------------------------------------------

/// Kad 联系点（`BootstrapRes`/`Res` 里的 Entry，nodes.dat 一条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub id: KadId,
    pub endpoint: Endpoint,
    pub version: u8,
}

impl Contact {
    fn decode(reader: &mut ByteReader<'_>) -> Option<Self> {
        let id = KadId::decode(reader)?;
        let endpoint = Endpoint::decode(reader)?;
        let version = reader.u8()?;
        Some(Contact {
            id,
            endpoint,
            version,
        })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        self.id.encode(out);
        self.endpoint.encode(out);
        out.push(self.version);
    }
}

// ---------------------------------------------------------------------------
// Tag（可变）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum TagValue {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    Str(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Tag {
    id: u8,
    value: TagValue,
}

impl Tag {
    /// 解码单个 tag。兼容两种名字约定：
    /// - **packed**（type 高位 0x80 置位）：`type|0x80 + id(1B) + value`；
    /// - **标准 ed2k**（真实 eMule 的 `KADEMLIA2_SEARCH_RES` 实测格式）：
    ///   `type + name_len(u16 LE) + name + value`，单字节名即 tag ID，
    ///   多字节名（如字符串型文件名 tag）保留值但 ID 记 0（不参与源提取）。
    fn decode(reader: &mut ByteReader<'_>) -> Option<Self> {
        let type_byte = reader.u8()?;
        let ty = type_byte & 0x7f;
        let id = if type_byte & 0x80 != 0 {
            reader.u8()?
        } else {
            let name_len = reader.u16_le()? as usize;
            let name = reader.take(name_len)?;
            if name_len == 1 { name[0] } else { 0 }
        };
        let value = match ty {
            TAG_TYPE_STRING => {
                let len = reader.u16_le()? as usize;
                let bytes = reader.take(len)?;
                TagValue::Str(bytes.to_vec())
            }
            TAG_TYPE_UINT8 => TagValue::U8(reader.u8()?),
            TAG_TYPE_UINT16 => TagValue::U16(reader.u16_le()?),
            TAG_TYPE_UINT32 => TagValue::U32(reader.u32_le()?),
            TAG_TYPE_UINT64 => TagValue::U64(reader.u64_le()?),
            t if (TAG_TYPE_STR1..=TAG_TYPE_STR16).contains(&t) => {
                let len = (t - TAG_TYPE_STR1 + 1) as usize;
                let bytes = reader.take(len)?;
                TagValue::Str(bytes.to_vec())
            }
            _ => return None,
        };
        Some(Tag { id, value })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        let (ty, mut payload): (u8, Vec<u8>) = match &self.value {
            TagValue::U8(v) => (TAG_TYPE_UINT8, vec![*v]),
            TagValue::U16(v) => (TAG_TYPE_UINT16, v.to_le_bytes().to_vec()),
            TagValue::U32(v) => (TAG_TYPE_UINT32, v.to_le_bytes().to_vec()),
            TagValue::U64(v) => (TAG_TYPE_UINT64, v.to_le_bytes().to_vec()),
            TagValue::Str(s) => {
                let mut p = (s.len() as u16).to_le_bytes().to_vec();
                p.extend_from_slice(s);
                (TAG_TYPE_STRING, p)
            }
        };
        out.push(ty | 0x80);
        out.push(self.id);
        out.append(&mut payload);
    }

    /// 取无符号数值（任何整型 tag）。
    fn as_u64(&self) -> Option<u64> {
        match self.value {
            TagValue::U8(v) => Some(u64::from(v)),
            TagValue::U16(v) => Some(u64::from(v)),
            TagValue::U32(v) => Some(u64::from(v)),
            TagValue::U64(v) => Some(v),
            TagValue::Str(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// SearchEntry（源条目）
// ---------------------------------------------------------------------------

/// `SearchRes` 里的一条结果：`KadID(16) + tagCount(1B) + tagCount×Tag`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    pub id: KadId,
    tags: Vec<Tag>,
}

impl SearchEntry {
    fn decode(reader: &mut ByteReader<'_>) -> Option<Self> {
        let id = KadId::decode(reader)?;
        let count = reader.u8()? as usize;
        let mut tags = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            let Some(tag) = Tag::decode(reader) else {
                // 畸形 tag：停止解析，保留已解出的（防越界）。
                break;
            };
            tags.push(tag);
        }
        Some(SearchEntry { id, tags })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        self.id.encode(out);
        out.push(self.tags.len().min(255) as u8);
        for tag in self.tags.iter().take(255) {
            tag.encode(out);
        }
    }

    /// 提取可连接的 HighID 源。要求 `SourceType ∈ {0,1,4}`，且有非零 IP+端口。
    pub fn source_peer(&self) -> Option<PeerAddr> {
        let mut source_type: Option<u64> = None;
        let mut ip: Option<u32> = None;
        let mut port: Option<u16> = None;
        for tag in &self.tags {
            match tag.id {
                TAG_SOURCE_TYPE => source_type = tag.as_u64(),
                TAG_SOURCE_IP => ip = tag.as_u64().map(|v| v as u32),
                TAG_SOURCE_PORT => port = tag.as_u64().map(|v| v as u16),
                _ => {}
            }
        }
        // SourceType 合法值：0/1/4（其余如 Kad-only 中转源不可直连）。
        match source_type {
            Some(0) | Some(1) | Some(4) => {}
            _ => return None,
        }
        let ip = ip?;
        let port = port?;
        if ip == 0 || port == 0 {
            return None;
        }
        // TAG_SOURCE_IP 值为主机序 u32（发布方 eMule 直接写 GetIPAddress()），
        // `Ipv4Addr::from(u32)` 即按大端语义还原 a.b.c.d。
        let addr = Ipv4Addr::from(ip);
        if addr.is_unspecified() || addr.is_broadcast() {
            return None;
        }
        Some(PeerAddr { ip: addr, port })
    }
}

// ---------------------------------------------------------------------------
// 帧封装 / 解析
// ---------------------------------------------------------------------------

/// 封装明文 Kad 帧：`<0xE4><opcode><payload>`。
pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + payload.len());
    out.push(PROTO_KAD);
    out.push(opcode);
    out.extend_from_slice(payload);
    out
}

/// 解析入站 datagram → `(proto, opcode, payload)`。仅接受明文 `0xE4`；
/// packed `0xE5` 与未知帧头返回 `None`（调用方丢弃）。
pub fn parse_frame(datagram: &[u8]) -> Option<(u8, u8, &[u8])> {
    if datagram.len() < 2 {
        return None;
    }
    let proto = datagram[0];
    if proto != PROTO_KAD {
        return None;
    }
    Some((proto, datagram[1], &datagram[2..]))
}

/// 解析入站 datagram（含 packed 变体）→ `(opcode, payload)`。
///
/// 明文 `0xE4` 零拷贝借用；`0xE5` 的 payload 经 zlib 限长解压（上限
/// `max_decompressed`，防炸弹）。eMule 对较大的 Kad 回包（典型如
/// `KADEMLIA2_SEARCH_RES`）一律压缩发送，只认明文会静默丢掉全部搜索结果。
pub fn parse_datagram(
    datagram: &[u8],
    max_decompressed: usize,
) -> Option<(u8, std::borrow::Cow<'_, [u8]>)> {
    if datagram.len() < 2 {
        return None;
    }
    match datagram[0] {
        PROTO_KAD => Some((datagram[1], std::borrow::Cow::Borrowed(&datagram[2..]))),
        PROTO_KAD_PACKED => {
            let payload =
                crate::ed2k::proto::decompress_bounded(&datagram[2..], max_decompressed).ok()?;
            Some((datagram[1], std::borrow::Cow::Owned(payload)))
        }
        _ => None,
    }
}

// --- 编码：请求 ---

/// `BootstrapReq`（空 payload）。
pub fn build_bootstrap_req() -> Vec<u8> {
    frame(OP_BOOTSTRAP_REQ, &[])
}

/// `HelloReq`：`KadID(16)+TCPPort(u16)+Version(1)+tagCount(1=0)`（leech 无 tag）。
pub fn build_hello_req(self_id: &KadId, tcp_port: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(20);
    self_id.encode(&mut p);
    p.extend_from_slice(&tcp_port.to_le_bytes());
    p.push(KADEMLIA_VERSION);
    p.push(0); // tagCount
    frame(OP_HELLO_REQ, &p)
}

/// `Req{FindNode}`：`SearchType(1)+Target(16)+Receiver(16)`。
pub fn build_find_node_req(target: &KadId, receiver: &KadId) -> Vec<u8> {
    let mut p = Vec::with_capacity(33);
    p.push(SEARCH_FIND_NODE);
    target.encode(&mut p);
    receiver.encode(&mut p);
    frame(OP_REQ, &p)
}

/// `SearchSourcesReq`：`Target(16)+StartPos(u16=0)+Size(u64)`。
pub fn build_search_sources_req(target: &KadId, size: u64) -> Vec<u8> {
    let mut p = Vec::with_capacity(26);
    target.encode(&mut p);
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&size.to_le_bytes());
    frame(OP_SEARCH_SRC_REQ, &p)
}

// --- 编码：应答（测试/回声用）---

/// `BootstrapRes`：`KadID(16)+TCPPort(u16)+Version(1)+count(u16)+count×Entry`。
pub fn build_bootstrap_res(self_id: &KadId, tcp_port: u16, contacts: &[Contact]) -> Vec<u8> {
    let mut p = Vec::new();
    self_id.encode(&mut p);
    p.extend_from_slice(&tcp_port.to_le_bytes());
    p.push(KADEMLIA_VERSION);
    p.extend_from_slice(&(contacts.len() as u16).to_le_bytes());
    for c in contacts {
        c.encode(&mut p);
    }
    frame(OP_BOOTSTRAP_RES, &p)
}

/// `Res`：`Target(16)+count(1)+count×Entry`。
pub fn build_find_node_res(target: &KadId, contacts: &[Contact]) -> Vec<u8> {
    let mut p = Vec::new();
    target.encode(&mut p);
    p.push(contacts.len().min(255) as u8);
    for c in contacts.iter().take(255) {
        c.encode(&mut p);
    }
    frame(OP_RES, &p)
}

/// `SearchRes`：`Source(16)+Target(16)+count(u16)+count×SearchEntry`。
pub fn build_search_res(source: &KadId, target: &KadId, entries: &[SearchEntry]) -> Vec<u8> {
    let mut p = Vec::new();
    source.encode(&mut p);
    target.encode(&mut p);
    p.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for e in entries {
        e.encode(&mut p);
    }
    frame(OP_SEARCH_RES, &p)
}

// --- 解码：应答 ---

/// 解码 `BootstrapRes` payload → 联系点列表（count 按剩余字节 clamp）。
pub fn decode_bootstrap_res(payload: &[u8]) -> Option<Vec<Contact>> {
    let mut r = ByteReader::new(payload);
    let _self_id = KadId::decode(&mut r)?;
    let _tcp_port = r.u16_le()?;
    let _version = r.u8()?;
    let declared = r.u16_le()? as usize;
    let max = r.remaining() / CONTACT_WIRE_SIZE;
    let count = declared.min(max);
    let mut contacts = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(c) = Contact::decode(&mut r) else {
            break;
        };
        contacts.push(c);
    }
    Some(contacts)
}

/// 应答包开头自报的节点身份（`BootstrapRes` / `HelloRes` 共用的头部）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeHeader {
    pub id: KadId,
    pub tcp_port: u16,
    pub version: u8,
}

/// 解码 `BootstrapRes` / `HelloRes` 开头的 `KadID(16)+TCPPort(u16)+Version(1)`。
pub fn decode_node_header(payload: &[u8]) -> Option<NodeHeader> {
    let mut r = ByteReader::new(payload);
    Some(NodeHeader {
        id: KadId::decode(&mut r)?,
        tcp_port: r.u16_le()?,
        version: r.u8()?,
    })
}

/// 解码 `Res`（FindNode 应答）payload → 联系点列表。
pub fn decode_find_node_res(payload: &[u8]) -> Option<Vec<Contact>> {
    let mut r = ByteReader::new(payload);
    let _target = KadId::decode(&mut r)?;
    let declared = r.u8()? as usize;
    let max = r.remaining() / CONTACT_WIRE_SIZE;
    let count = declared.min(max);
    let mut contacts = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(c) = Contact::decode(&mut r) else {
            break;
        };
        contacts.push(c);
    }
    Some(contacts)
}

/// 解码 `SearchRes` payload → 源条目列表。
pub fn decode_search_res(payload: &[u8]) -> Option<Vec<SearchEntry>> {
    let mut r = ByteReader::new(payload);
    let _source = KadId::decode(&mut r)?;
    let _target = KadId::decode(&mut r)?;
    let declared = r.u16_le()? as usize;
    // 每条 SearchEntry 至少 KadID(16)+count(1)=17 字节。
    let max = r.remaining() / 17 + 1;
    let count = declared.min(max);
    let mut entries = Vec::with_capacity(count.min(512));
    for _ in 0..count {
        let Some(e) = SearchEntry::decode(&mut r) else {
            break;
        };
        entries.push(e);
    }
    Some(entries)
}

// ---------------------------------------------------------------------------
// nodes.dat 解析
// ---------------------------------------------------------------------------

/// 从 `nodes.dat` 原始字节解析 bootstrap 联系点。
///
/// 格式：
/// - `first = u32 LE`；`first != 0` → 旧格式，`numContacts = first`，version=0。
/// - `first == 0` → `version = u32 LE`（1..=3）；version>=3 时先读
///   `bootstrapEdition = u32 LE`；再读 `numContacts = u32 LE`。
/// - 每条：Contact(25B)；version>=2 且 edition==0 时额外读 8B(KadUDPKey 忽略)+1B(verified)。
///
/// 畸形 count 按剩余字节 clamp，不 panic、不超量分配。
pub fn parse_nodes_dat(bytes: &[u8]) -> Vec<Contact> {
    let mut r = ByteReader::new(bytes);
    let Some(first) = r.u32_le() else {
        return Vec::new();
    };

    let mut version: u32 = 0;
    let mut bootstrap_edition: u32 = 0;

    let num_contacts = if first != 0 {
        first
    } else {
        let Some(v) = r.u32_le() else {
            return Vec::new();
        };
        version = v;
        if !(1..=3).contains(&version) {
            return Vec::new();
        }
        if version >= 3 {
            let Some(edition) = r.u32_le() else {
                return Vec::new();
            };
            bootstrap_edition = edition;
        }
        let Some(n) = r.u32_le() else {
            return Vec::new();
        };
        n
    };

    let extra = if version >= 2 && bootstrap_edition == 0 {
        9 // KadUDPKey(8) + verified(1)
    } else {
        0
    };
    let entry_size = CONTACT_WIRE_SIZE + extra;
    let max = r.remaining() / entry_size.max(1);
    let count = (num_contacts as usize).min(max);

    let mut contacts = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(c) = Contact::decode(&mut r) else {
            break;
        };
        if extra > 0 && r.take(extra).is_none() {
            break;
        }
        contacts.push(c);
    }
    contacts
}

/// 把联系点编码为旧格式 `nodes.dat`（`count(u32 LE) + count × Contact(25B)`），
/// [`parse_nodes_dat`] 可原样读回。用于缓存多个订阅源合并后的引导联系点。
///
/// 旧格式以 `count != 0` 与新格式区分，无法表示 0 条：空列表编码为空字节。
pub fn encode_nodes_dat(contacts: &[Contact]) -> Vec<u8> {
    let Ok(count) = u32::try_from(contacts.len()) else {
        return Vec::new();
    };
    if count == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(4 + contacts.len() * CONTACT_WIRE_SIZE);
    out.extend_from_slice(&count.to_le_bytes());
    for contact in contacts {
        contact.encode(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        CONTACT_WIRE_SIZE, Contact, Endpoint, KADEMLIA_VERSION, KadId, OP_HELLO_REQ, OP_REQ,
        OP_SEARCH_SRC_REQ, PROTO_KAD, PROTO_KAD_PACKED, SEARCH_FIND_NODE, SearchEntry,
        TAG_SOURCE_IP, TAG_SOURCE_PORT, TAG_SOURCE_TYPE, Tag, TagValue, build_bootstrap_res,
        build_find_node_req, build_find_node_res, build_hello_req, build_search_res,
        build_search_sources_req, decode_bootstrap_res, decode_find_node_res, decode_search_res,
        dword_reverse, parse_frame, parse_nodes_dat,
    };
    use std::net::Ipv4Addr;

    fn sample_id(seed: u8) -> KadId {
        let mut b = [0u8; 16];
        for (i, x) in b.iter_mut().enumerate() {
            *x = seed.wrapping_add(i as u8);
        }
        KadId(b)
    }

    fn sample_contact(seed: u8) -> Contact {
        Contact {
            id: sample_id(seed),
            endpoint: Endpoint {
                ip: Ipv4Addr::new(1, 2, 3, 4),
                udp_port: 4672,
                tcp_port: 4662,
            },
            version: KADEMLIA_VERSION,
        }
    }

    #[test]
    fn dword_reverse_is_involution() {
        let raw: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        assert_eq!(dword_reverse(&dword_reverse(&raw)), raw);
    }

    #[test]
    fn dword_reverse_known_vector() {
        // 每 4 字节 dword 内反转。
        let raw: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let got = dword_reverse(&raw);
        let want: [u8; 16] = [3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12];
        assert_eq!(got, want);
    }

    #[test]
    fn kadid_wire_roundtrip() {
        let id = sample_id(0x42);
        let mut out = Vec::new();
        id.encode(&mut out);
        // encode 应用 dword 反转。
        assert_eq!(out.as_slice(), &dword_reverse(&id.0));
        let mut r = super::ByteReader::new(&out);
        let back = KadId::decode(&mut r).expect_none_ok();
        assert_eq!(back, id);
    }

    // 辅助：测试里替代 unwrap（clippy deny unwrap_used）。
    trait ExpectNoneOk<T> {
        fn expect_none_ok(self) -> T;
    }
    impl<T> ExpectNoneOk<T> for Option<T> {
        fn expect_none_ok(self) -> T {
            match self {
                Some(v) => v,
                None => panic!("expected Some"),
            }
        }
    }

    #[test]
    fn endpoint_roundtrip_and_ip_order() {
        let ep = Endpoint {
            ip: Ipv4Addr::new(1, 2, 3, 4),
            udp_port: 0x1234,
            tcp_port: 0x5678,
        };
        let mut out = Vec::new();
        ep.encode(&mut out);
        // IP 主机序 u32 小端：字节反序 [4,3,2,1]。
        assert_eq!(&out[0..4], &[4, 3, 2, 1]);
        // 端口小端。
        assert_eq!(&out[4..6], &0x1234u16.to_le_bytes());
        assert_eq!(&out[6..8], &0x5678u16.to_le_bytes());
        let mut r = super::ByteReader::new(&out);
        let back = Endpoint::decode(&mut r).expect_none_ok();
        assert_eq!(back, ep);
    }

    #[test]
    fn contact_wire_size_is_25() {
        let mut out = Vec::new();
        sample_contact(1).encode(&mut out);
        assert_eq!(out.len(), CONTACT_WIRE_SIZE);
    }

    #[test]
    fn contact_roundtrip() {
        let c = sample_contact(0x11);
        let mut out = Vec::new();
        c.encode(&mut out);
        let mut r = super::ByteReader::new(&out);
        let back = Contact::decode(&mut r).expect_none_ok();
        assert_eq!(back, c);
    }

    #[test]
    fn tag_roundtrip_all_types() {
        let tags = [
            Tag {
                id: 0x01,
                value: TagValue::U8(0xAB),
            },
            Tag {
                id: 0x02,
                value: TagValue::U16(0xBEEF),
            },
            Tag {
                id: 0x03,
                value: TagValue::U32(0xDEADBEEF),
            },
            Tag {
                id: 0x04,
                value: TagValue::U64(0x0102030405060708),
            },
            Tag {
                id: 0x05,
                value: TagValue::Str(b"hello".to_vec()),
            },
        ];
        for t in &tags {
            let mut out = Vec::new();
            t.encode(&mut out);
            let mut r = super::ByteReader::new(&out);
            let back = Tag::decode(&mut r).expect_none_ok();
            assert_eq!(&back, t);
        }
    }

    #[test]
    fn tag_without_id_bit_rejected() {
        // type 高位未置位 → 畸形。
        let buf = [0x03u8, 0x01, 0, 0, 0, 0];
        let mut r = super::ByteReader::new(&buf);
        assert!(Tag::decode(&mut r).is_none());
    }

    #[test]
    fn search_entry_roundtrip() {
        let e = SearchEntry {
            id: sample_id(7),
            tags: vec![
                Tag {
                    id: TAG_SOURCE_TYPE,
                    value: TagValue::U8(1),
                },
                Tag {
                    id: TAG_SOURCE_IP,
                    value: TagValue::U32(u32::from_le_bytes([9, 8, 7, 6])),
                },
                Tag {
                    id: TAG_SOURCE_PORT,
                    value: TagValue::U16(4662),
                },
            ],
        };
        let mut out = Vec::new();
        e.encode(&mut out);
        let mut r = super::ByteReader::new(&out);
        let back = SearchEntry::decode(&mut r).expect_none_ok();
        assert_eq!(back, e);
    }

    #[test]
    fn source_peer_valid() {
        let e = SearchEntry {
            id: sample_id(3),
            tags: vec![
                Tag {
                    id: TAG_SOURCE_TYPE,
                    value: TagValue::U8(1),
                },
                Tag {
                    id: TAG_SOURCE_IP,
                    value: TagValue::U32(u32::from_be_bytes([1, 2, 3, 4])),
                },
                Tag {
                    id: TAG_SOURCE_PORT,
                    value: TagValue::U16(4662),
                },
            ],
        };
        let peer = e.source_peer().expect_none_ok();
        assert_eq!(peer.ip, Ipv4Addr::new(1, 2, 3, 4));
        assert_eq!(peer.port, 4662);
    }

    #[test]
    fn source_peer_bad_type_skipped() {
        let e = SearchEntry {
            id: sample_id(3),
            tags: vec![
                Tag {
                    id: TAG_SOURCE_TYPE,
                    value: TagValue::U8(3),
                }, // 非法
                Tag {
                    id: TAG_SOURCE_IP,
                    value: TagValue::U32(u32::from_le_bytes([1, 2, 3, 4])),
                },
                Tag {
                    id: TAG_SOURCE_PORT,
                    value: TagValue::U16(4662),
                },
            ],
        };
        assert!(e.source_peer().is_none());
    }

    #[test]
    fn source_peer_missing_ip_skipped() {
        let e = SearchEntry {
            id: sample_id(3),
            tags: vec![Tag {
                id: TAG_SOURCE_TYPE,
                value: TagValue::U8(1),
            }],
        };
        assert!(e.source_peer().is_none());
    }

    #[test]
    fn frame_and_parse() {
        let f = build_hello_req(&sample_id(1), 4662);
        assert_eq!(f[0], PROTO_KAD);
        assert_eq!(f[1], OP_HELLO_REQ);
        let (proto, op, payload) = parse_frame(&f).expect_none_ok();
        assert_eq!(proto, PROTO_KAD);
        assert_eq!(op, OP_HELLO_REQ);
        assert!(!payload.is_empty());
    }

    #[test]
    fn packed_frame_dropped() {
        let buf = [PROTO_KAD_PACKED, 0x09, 1, 2, 3];
        assert!(parse_frame(&buf).is_none());
    }

    #[test]
    fn find_node_req_layout() {
        let target = sample_id(0xAA);
        let recv = sample_id(0xBB);
        let f = build_find_node_req(&target, &recv);
        assert_eq!(f[1], OP_REQ);
        assert_eq!(f[2], SEARCH_FIND_NODE);
        assert_eq!(f.len(), 2 + 1 + 16 + 16);
    }

    #[test]
    fn search_sources_req_layout() {
        let target = sample_id(0xCC);
        let f = build_search_sources_req(&target, 0x0102030405060708);
        assert_eq!(f[1], OP_SEARCH_SRC_REQ);
        // 2(帧头) + 16(target) + 2(startpos) + 8(size)
        assert_eq!(f.len(), 2 + 16 + 2 + 8);
        // size 小端在末尾。
        assert_eq!(&f[f.len() - 8..], &0x0102030405060708u64.to_le_bytes());
    }

    #[test]
    fn bootstrap_res_roundtrip() {
        let contacts = vec![sample_contact(1), sample_contact(50), sample_contact(200)];
        let f = build_bootstrap_res(&sample_id(0), 4662, &contacts);
        let (_p, _op, payload) = parse_frame(&f).expect_none_ok();
        let back = decode_bootstrap_res(payload).expect_none_ok();
        assert_eq!(back, contacts);
    }

    #[test]
    fn find_node_res_roundtrip() {
        let target = sample_id(9);
        let contacts = vec![sample_contact(2), sample_contact(3)];
        let f = build_find_node_res(&target, &contacts);
        let (_p, _op, payload) = parse_frame(&f).expect_none_ok();
        let back = decode_find_node_res(payload).expect_none_ok();
        assert_eq!(back, contacts);
    }

    #[test]
    fn search_res_roundtrip() {
        let src = sample_id(1);
        let target = sample_id(2);
        let entries = vec![SearchEntry {
            id: sample_id(5),
            tags: vec![
                Tag {
                    id: TAG_SOURCE_TYPE,
                    value: TagValue::U8(1),
                },
                Tag {
                    id: TAG_SOURCE_IP,
                    value: TagValue::U32(u32::from_le_bytes([5, 6, 7, 8])),
                },
                Tag {
                    id: TAG_SOURCE_PORT,
                    value: TagValue::U16(1234),
                },
            ],
        }];
        let f = build_search_res(&src, &target, &entries);
        let (_p, _op, payload) = parse_frame(&f).expect_none_ok();
        let back = decode_search_res(payload).expect_none_ok();
        assert_eq!(back, entries);
    }

    #[test]
    fn decode_bootstrap_res_clamps_liar_count() {
        // 声明 65535 条但只给 1 条的字节 → 不 panic，返回 1 条。
        let mut payload = Vec::new();
        sample_id(0).encode(&mut payload);
        payload.extend_from_slice(&4662u16.to_le_bytes());
        payload.push(KADEMLIA_VERSION);
        payload.extend_from_slice(&65535u16.to_le_bytes());
        sample_contact(1).encode(&mut payload);
        let back = decode_bootstrap_res(&payload).expect_none_ok();
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn xor_cmp_direction() {
        let target = KadId([0u8; 16]);
        let near = KadId([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let far = KadId([0xFF; 16]);
        assert_eq!(near.xor_cmp(&far, &target), std::cmp::Ordering::Less);
        assert_eq!(far.xor_cmp(&near, &target), std::cmp::Ordering::Greater);
        assert_eq!(near.xor_cmp(&near, &target), std::cmp::Ordering::Equal);
    }

    #[test]
    fn nodes_dat_old_format() {
        // first != 0 → 旧格式，first 条联系点，无 verified。
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u32.to_le_bytes());
        sample_contact(1).encode(&mut buf);
        sample_contact(2).encode(&mut buf);
        let contacts = parse_nodes_dat(&buf);
        assert_eq!(contacts.len(), 2);
    }

    #[test]
    fn nodes_dat_v2_with_verified() {
        // first==0, version=2, edition=0 → 每条 +8(key)+1(verified)。
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes()); // numContacts
        sample_contact(9).encode(&mut buf);
        buf.extend_from_slice(&[0u8; 8]); // KadUDPKey
        buf.push(1); // verified
        let contacts = parse_nodes_dat(&buf);
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].endpoint.ip, Ipv4Addr::new(1, 2, 3, 4));
    }

    #[test]
    fn nodes_dat_malformed_count_no_panic() {
        // 声明海量 count 但只有几字节 → 空/截断，绝不 panic。
        let mut buf = Vec::new();
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        buf.extend_from_slice(&[1, 2, 3]);
        let contacts = parse_nodes_dat(&buf);
        assert!(contacts.len() <= 1);
    }

    #[test]
    fn nodes_dat_empty_no_panic() {
        assert!(parse_nodes_dat(&[]).is_empty());
        assert!(parse_nodes_dat(&[1, 2]).is_empty());
    }
}
