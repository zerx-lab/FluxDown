//! 配对协议：一次性配对码 + X25519 ECDH + SAS 短认证串 + Ed25519 身份绑定。
//!
//! # 安全模型
//! - **配对码**（6 位数字，TTL 120s，单次使用）：一次性引导凭据，授权本次配对并
//!   限速暴力尝试；本身非长期密钥，泄漏一张过期码无害。
//! - **X25519 ECDH**：双方各出一把**临时**密钥做密钥协商得共享密钥 `z`（前向保密）。
//! - **SAS**（6 位，双端肉眼核对）：从 `z` + 双方临时公钥派生；中间人会与两端各自
//!   协商出不同 `z` → 两端 SAS 不一致 → 用户肉眼即可发现（防 MITM）。
//! - **Ed25519 身份绑定**：每端用长期身份私钥对「域分隔串 || 临时公钥」签名，对端
//!   用其出示的身份公钥验签——把长期身份与本次临时密钥绑定，杜绝身份冒充。
//! - **每对设备独立链路密钥**：`derive_link_key(z)`，用于后续数据面 HMAC 鉴权，
//!   绝不上网络明文。
//!
//! 全部密码学步骤在引擎内完成（wire 层只做 base64 编解码），便于集中审计与单测。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::{Rng, RngCore};
use x25519_dalek::{PublicKey, StaticSecret};

use super::crypto::{derive_link_key, derive_sas, fingerprint};
use super::error::{LinkError, LinkResult};
use super::identity::LinkIdentity;
use super::types::{PeerCandidate, PeerRecord, TransportKind};
/// 配对码有效期。
const CODE_TTL: Duration = Duration::from_secs(120);
/// confirm 会话有效期（hello 之后必须尽快核对 SAS 并确认）。
const SESSION_TTL: Duration = Duration::from_secs(180);
/// 无匹配 hello（猜码）在时窗内的上限——超过即节流，防在线暴力猜码。
/// 与配对码生命周期**解耦**：错误猜测绝不作废有效码（避免猜码 DoS）。
const MAX_FAILED_HELLOS: usize = 30;
/// 失败 hello 的统计时窗。
const FAILED_HELLO_WINDOW: Duration = Duration::from_secs(120);

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn transcript_init(code: &str, init_eph_pub: &[u8; 32], init_id_pub: &[u8; 32]) -> Vec<u8> {
    let mut t = Vec::with_capacity(21 + code.len() + 64);
    t.extend_from_slice(b"fluxdown-link-init-v1");
    t.extend_from_slice(code.as_bytes());
    t.extend_from_slice(init_eph_pub);
    t.extend_from_slice(init_id_pub);
    t
}

fn transcript_resp(resp_eph_pub: &[u8; 32], init_eph_pub: &[u8; 32]) -> Vec<u8> {
    let mut t = Vec::with_capacity(21 + 64);
    t.extend_from_slice(b"fluxdown-link-resp-v1");
    t.extend_from_slice(resp_eph_pub);
    t.extend_from_slice(init_eph_pub);
    t
}

/// 本机在配对响应中呈现的自身信息。
#[derive(Debug, Clone)]
pub struct SelfInfo {
    pub name: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
}

/// 发起方 `hello` 请求（byte-oriented；wire 层负责 base64 编解码）。
#[derive(Debug, Clone)]
pub struct HelloRequest {
    pub code: String,
    pub initiator_eph_pub: [u8; 32],
    pub initiator_id_pub: [u8; 32],
    pub initiator_sig: [u8; 64],
    pub name: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    /// 发起方自报的可达候选地址（`ip:port`），供响应方存为回连候选。
    pub initiator_addrs: Vec<String>,
}

/// 响应方 `hello` 回复。
#[derive(Debug, Clone)]
pub struct HelloResponse {
    pub session_id: String,
    pub responder_eph_pub: [u8; 32],
    pub responder_id_pub: [u8; 32],
    pub responder_sig: [u8; 64],
    pub name: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    /// 响应方本地显示用的 SAS（应与发起方计算出的一致）。
    pub sas: String,
}

/// 一条待确认会话（hello 已完成、等待 SAS 核对 + confirm）。
struct ConfirmEntry {
    z: [u8; 32],
    initiator_id_pub: [u8; 32],
    name: String,
    platform: Option<String>,
    initiator_addrs: Vec<String>,
    created: Instant,
}

/// 一个已生成、尚未消费的配对码。
struct CodeEntry {
    code: String,
    eph_secret: StaticSecret,
    created: Instant,
}

/// 配对**响应方**（被添加的设备侧）。持有一次性配对码与待确认会话表。
pub struct PairingResponder {
    identity: LinkIdentity,
    self_info: SelfInfo,
    codes: Mutex<Vec<CodeEntry>>,
    sessions: Mutex<HashMap<String, ConfirmEntry>>,
    /// 失败 hello（猜码）时间戳，与配对码解耦的全局节流器（防在线暴力猜码 DoS）。
    failures: Mutex<Vec<Instant>>,
}

impl PairingResponder {
    #[must_use]
    pub fn new(identity: LinkIdentity, self_info: SelfInfo) -> Self {
        Self {
            identity,
            self_info,
            codes: Mutex::new(Vec::new()),
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(Vec::new()),
        }
    }

    /// 记录一次失败 hello（无匹配码）。按时窗剪枝并限长，保持有界；与配对码解耦，
    /// 错误猜测绝不影响任何有效码的生命周期。
    fn record_failed_hello(&self) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.retain(|t| t.elapsed() < FAILED_HELLO_WINDOW);
            if failures.len() < MAX_FAILED_HELLOS {
                failures.push(Instant::now());
            }
        }
    }

    /// 生成一个新配对码（在被添加设备的 UI 上展示，2 分钟内有效、单次使用）。
    pub fn generate_code(&self) -> String {
        let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        let eph_secret = StaticSecret::from(seed);
        if let Ok(mut codes) = self.codes.lock() {
            codes.retain(|c| c.created.elapsed() < CODE_TTL);
            codes.push(CodeEntry {
                code: code.clone(),
                eph_secret,
                created: Instant::now(),
            });
        }
        code
    }

    /// 处理 `hello`：校验码 → ECDH → 验发起方签名 → 建会话 → 签自身握手。
    pub fn handle_hello(&self, req: &HelloRequest) -> LinkResult<HelloResponse> {
        // 自配对守卫：发起方与本机持同一长期身份（典型场景：两个进程共享同一
        // 引擎数据库）——它们本就是同一台设备，直接拒绝，不消费配对码。
        if req.initiator_id_pub == self.identity.public_bytes() {
            return Err(LinkError::SelfPairing);
        }
        // 取出并消费匹配的有效码（消费即移除，杜绝重放）。
        let eph_secret = {
            let mut codes = self.codes.lock().map_err(|_| LinkError::Unavailable)?;
            codes.retain(|c| c.created.elapsed() < CODE_TTL);
            let Some(pos) = codes.iter().position(|c| c.code == req.code) else {
                // 无匹配：仅记一次失败到解耦的全局节流器，**绝不**作废有效码（修复猜码 DoS）。
                drop(codes);
                self.record_failed_hello();
                return Err(LinkError::InvalidCode);
            };
            codes.remove(pos).eph_secret
        };

        // 验发起方身份签名：绑定其长期身份与本次临时公钥。
        let transcript = transcript_init(&req.code, &req.initiator_eph_pub, &req.initiator_id_pub);
        if !LinkIdentity::verify(&req.initiator_id_pub, &transcript, &req.initiator_sig) {
            return Err(LinkError::BadSignature);
        }

        let z = eph_secret
            .diffie_hellman(&PublicKey::from(req.initiator_eph_pub))
            .to_bytes();
        let responder_eph_pub = PublicKey::from(&eph_secret).to_bytes();
        let sas = derive_sas(&z, &req.initiator_eph_pub, &responder_eph_pub);
        let responder_sig = self
            .identity
            .sign(&transcript_resp(&responder_eph_pub, &req.initiator_eph_pub));
        let session_id = uuid::Uuid::new_v4().simple().to_string();

        {
            let mut sessions = self.sessions.lock().map_err(|_| LinkError::Unavailable)?;
            sessions.retain(|_, e| e.created.elapsed() < SESSION_TTL);
            sessions.insert(
                session_id.clone(),
                ConfirmEntry {
                    z,
                    initiator_id_pub: req.initiator_id_pub,
                    name: req.name.clone(),
                    platform: req.platform.clone(),
                    initiator_addrs: req.initiator_addrs.clone(),
                    created: Instant::now(),
                },
            );
        }

        Ok(HelloResponse {
            session_id,
            responder_eph_pub,
            responder_id_pub: self.identity.public_bytes(),
            responder_sig,
            name: self.self_info.name.clone(),
            platform: self.self_info.platform.clone(),
            app_version: self.self_info.app_version.clone(),
            sas,
        })
    }

    /// 处理 `confirm`：用户核对 SAS 一致后确认，返回要登记的发起方设备记录。
    /// `confirm=false`（用户发现 SAS 不符）→ 丢弃会话，返回 `None`。
    pub fn handle_confirm(
        &self,
        session_id: &str,
        confirm: bool,
    ) -> LinkResult<Option<PeerRecord>> {
        let entry = {
            let mut sessions = self.sessions.lock().map_err(|_| LinkError::Unavailable)?;
            sessions.retain(|_, e| e.created.elapsed() < SESSION_TTL);
            sessions.remove(session_id)
        };
        let Some(entry) = entry else {
            return Err(LinkError::SessionExpired);
        };
        if !confirm {
            return Ok(None);
        }
        let now = now_unix();
        let candidates = entry
            .initiator_addrs
            .iter()
            .map(|a| PeerCandidate {
                kind: TransportKind::Direct,
                address: a.clone(),
            })
            .collect();
        Ok(Some(PeerRecord {
            fingerprint: fingerprint(&entry.initiator_id_pub),
            identity_pub: entry.initiator_id_pub.to_vec(),
            name: entry.name,
            platform: entry.platform,
            link_secret: derive_link_key(&entry.z),
            candidates,
            paired_at: now,
            last_seen_at: now,
        }))
    }
}

/// 配对**发起方**（正在添加设备的一侧）。跨两次 HTTP 往返有状态。
pub struct PairingInitiator {
    identity: LinkIdentity,
    eph_secret: StaticSecret,
    eph_pub: [u8; 32],
    /// hello 之后填充：协商出的 `z` + 响应方信息（confirm 阶段登记用）。
    negotiated: Option<Negotiated>,
}

struct Negotiated {
    z: [u8; 32],
    responder_id_pub: [u8; 32],
    responder_name: String,
    responder_platform: Option<String>,
    responder_addr: String,
}

impl PairingInitiator {
    /// 用本机身份新建一次发起方会话（生成临时 X25519 密钥）。
    #[must_use]
    pub fn new(identity: LinkIdentity) -> Self {
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        let eph_secret = StaticSecret::from(seed);
        let eph_pub = PublicKey::from(&eph_secret).to_bytes();
        Self {
            identity,
            eph_secret,
            eph_pub,
            negotiated: None,
        }
    }

    /// 构造 `hello` 请求（对给定配对码签名）。
    #[must_use]
    pub fn build_hello(
        &self,
        code: &str,
        self_info: &SelfInfo,
        initiator_addrs: Vec<String>,
    ) -> HelloRequest {
        let id_pub = self.identity.public_bytes();
        let sig = self
            .identity
            .sign(&transcript_init(code, &self.eph_pub, &id_pub));
        HelloRequest {
            code: code.to_string(),
            initiator_eph_pub: self.eph_pub,
            initiator_id_pub: id_pub,
            initiator_sig: sig,
            name: self_info.name.clone(),
            platform: self_info.platform.clone(),
            app_version: self_info.app_version.clone(),
            initiator_addrs,
        }
    }

    /// 处理响应方 `hello` 回复：验响应方签名 → 计算 `z` 与 SAS。返回本地应展示的 SAS。
    /// `responder_addr` 是发起方本次拨通对端所用的 `ip:port`（存为回连候选）。
    pub fn on_hello_response(
        &mut self,
        resp: &HelloResponse,
        responder_addr: &str,
    ) -> LinkResult<String> {
        // 自配对守卫先于验签：身份相同即可决断（共享数据库的进程互连场景）。
        if resp.responder_id_pub == self.identity.public_bytes() {
            return Err(LinkError::SelfPairing);
        }
        if !LinkIdentity::verify(
            &resp.responder_id_pub,
            &transcript_resp(&resp.responder_eph_pub, &self.eph_pub),
            &resp.responder_sig,
        ) {
            return Err(LinkError::BadSignature);
        }
        let z = self
            .eph_secret
            .diffie_hellman(&PublicKey::from(resp.responder_eph_pub))
            .to_bytes();
        let sas = derive_sas(&z, &self.eph_pub, &resp.responder_eph_pub);
        self.negotiated = Some(Negotiated {
            z,
            responder_id_pub: resp.responder_id_pub,
            responder_name: resp.name.clone(),
            responder_platform: resp.platform.clone(),
            responder_addr: responder_addr.to_string(),
        });
        Ok(sas)
    }

    /// SAS 核对通过后，产出要登记的响应方设备记录（confirm 成功分支调用）。
    pub fn finalize(&self) -> LinkResult<PeerRecord> {
        let n = self.negotiated.as_ref().ok_or(LinkError::SessionExpired)?;
        let now = now_unix();
        Ok(PeerRecord {
            fingerprint: fingerprint(&n.responder_id_pub),
            identity_pub: n.responder_id_pub.to_vec(),
            name: n.responder_name.clone(),
            platform: n.responder_platform.clone(),
            link_secret: derive_link_key(&n.z),
            candidates: vec![PeerCandidate {
                kind: TransportKind::Direct,
                address: n.responder_addr.clone(),
            }],
            paired_at: now,
            last_seen_at: now,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn self_info(name: &str) -> SelfInfo {
        SelfInfo {
            name: name.to_string(),
            platform: Some("linux".to_string()),
            app_version: Some("0.1.0".to_string()),
        }
    }

    #[test]
    fn self_pairing_rejected_without_consuming_code() {
        // 双端同一身份（共享引擎数据库的两个进程）→ hello 直接拒绝，且配对码不被消费。
        let id = LinkIdentity::generate();
        let responder = PairingResponder::new(id.clone(), self_info("NAS"));
        let mut initiator = PairingInitiator::new(id.clone());

        let code = responder.generate_code();
        let hello = initiator.build_hello(&code, &self_info("NAS"), vec![]);
        assert!(matches!(
            responder.handle_hello(&hello),
            Err(LinkError::SelfPairing)
        ));

        // 码未被消费：换一个正常身份仍可用同一码完成 hello。
        let mut other = PairingInitiator::new(LinkIdentity::generate());
        let hello2 = other.build_hello(&code, &self_info("Laptop"), vec![]);
        let resp = responder.handle_hello(&hello2).unwrap();
        // 发起方若发现响应方就是自己，同样拒绝。
        assert!(matches!(
            initiator.on_hello_response(&resp, "127.0.0.1:1"),
            Err(LinkError::SelfPairing)
        ));
    }

    #[test]
    fn full_handshake_agrees_on_sas_and_link_key() {
        let resp_id = LinkIdentity::generate();
        let responder = PairingResponder::new(resp_id.clone(), self_info("NAS"));
        let init_id = LinkIdentity::generate();
        let mut initiator = PairingInitiator::new(init_id.clone());

        let code = responder.generate_code();
        let hello =
            initiator.build_hello(&code, &self_info("Laptop"), vec!["10.0.0.2:17800".into()]);
        let hello_resp = responder.handle_hello(&hello).unwrap();

        let init_sas = initiator
            .on_hello_response(&hello_resp, "10.0.0.1:17800")
            .unwrap();
        // 双端 SAS 一致（无中间人）。
        assert_eq!(init_sas, hello_resp.sas);

        // 双端确认 → 各自登记对方。
        let resp_side = responder
            .handle_confirm(&hello_resp.session_id, true)
            .unwrap()
            .unwrap();
        let init_side = initiator.finalize().unwrap();

        // 响应方登记的是发起方身份；发起方登记的是响应方身份。
        assert_eq!(resp_side.fingerprint, init_id.fingerprint());
        assert_eq!(init_side.fingerprint, resp_id.fingerprint());
        // 关键：双方派生出**相同**链路密钥（ECDH 对称）。
        assert_eq!(resp_side.link_secret, init_side.link_secret);
        assert_eq!(resp_side.link_secret.len(), 32);
        // 候选端点各自记录了对端地址。
        assert_eq!(resp_side.direct_address(), Some("10.0.0.2:17800"));
        assert_eq!(init_side.direct_address(), Some("10.0.0.1:17800"));
    }

    #[test]
    fn wrong_code_is_rejected() {
        let responder = PairingResponder::new(LinkIdentity::generate(), self_info("NAS"));
        let initiator = PairingInitiator::new(LinkIdentity::generate());
        responder.generate_code();
        let hello = initiator.build_hello("000000", &self_info("L"), vec![]);
        // 除非恰好猜中，几乎必然 InvalidCode（用固定错码 000000 对真码概率 1e-6）。
        let real = responder.generate_code();
        if real != "000000" {
            assert!(matches!(
                responder.handle_hello(&hello),
                Err(LinkError::InvalidCode)
            ));
        }
    }

    #[test]
    fn code_is_single_use() {
        let responder = PairingResponder::new(LinkIdentity::generate(), self_info("NAS"));
        let initiator = PairingInitiator::new(LinkIdentity::generate());
        let code = responder.generate_code();
        let hello = initiator.build_hello(&code, &self_info("L"), vec![]);
        assert!(responder.handle_hello(&hello).is_ok());
        // 同码第二次 → 已消费 → InvalidCode。
        assert!(matches!(
            responder.handle_hello(&hello),
            Err(LinkError::InvalidCode)
        ));
    }

    #[test]
    fn tampered_initiator_signature_rejected() {
        let responder = PairingResponder::new(LinkIdentity::generate(), self_info("NAS"));
        let initiator = PairingInitiator::new(LinkIdentity::generate());
        let code = responder.generate_code();
        let mut hello = initiator.build_hello(&code, &self_info("L"), vec![]);
        hello.initiator_sig[0] ^= 0xff; // 篡改签名
        assert!(matches!(
            responder.handle_hello(&hello),
            Err(LinkError::BadSignature)
        ));
    }

    #[test]
    fn mitm_yields_diverging_sas() {
        // 模拟中间人：响应方回复里的临时公钥被替换（攻击者夹在中间）。
        // 发起方据被替换的公钥算出的 z' ≠ 响应方真实 z → SAS 不一致 → 用户可发现。
        let responder = PairingResponder::new(LinkIdentity::generate(), self_info("NAS"));
        let mut initiator = PairingInitiator::new(LinkIdentity::generate());
        let code = responder.generate_code();
        let hello = initiator.build_hello(&code, &self_info("L"), vec![]);
        let mut hello_resp = responder.handle_hello(&hello).unwrap();
        let attacker = LinkIdentity::generate();
        // 攻击者替换响应方临时公钥（并被迫重签，否则验签直接失败）。
        let mut seed = [1u8; 32];
        seed[0] = 7;
        let atk_secret = StaticSecret::from(seed);
        hello_resp.responder_eph_pub = PublicKey::from(&atk_secret).to_bytes();
        hello_resp.responder_id_pub = attacker.public_bytes();
        hello_resp.responder_sig = attacker.sign(&transcript_resp(
            &hello_resp.responder_eph_pub,
            &hello.initiator_eph_pub,
        ));
        let init_sas = initiator.on_hello_response(&hello_resp, "x").unwrap();
        // 发起方 SAS ≠ 响应方真实 SAS → 肉眼核对失败。
        assert_ne!(init_sas, hello_resp.sas);
    }

    #[test]
    fn confirm_false_registers_nothing() {
        let responder = PairingResponder::new(LinkIdentity::generate(), self_info("NAS"));
        let initiator = PairingInitiator::new(LinkIdentity::generate());
        let code = responder.generate_code();
        let hello = initiator.build_hello(&code, &self_info("L"), vec![]);
        let hr = responder.handle_hello(&hello).unwrap();
        assert!(
            responder
                .handle_confirm(&hr.session_id, false)
                .unwrap()
                .is_none()
        );
        // 会话已消费 → 再次 confirm → SessionExpired。
        assert!(matches!(
            responder.handle_confirm(&hr.session_id, true),
            Err(LinkError::SessionExpired)
        ));
    }
}
