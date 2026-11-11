//! WebSocket JSON-RPC 会话：daemon ↔ agent 握手门禁、首帧握手与慢方法调度。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use fluxdown_protocol::handshake::{
    AuthChallengeParams, AuthChallengeResult, AuthProveParams, AuthProveResult,
    SYSTEM_AUTH_CHALLENGE, SYSTEM_AUTH_PROVE, credentials_match, http_credential, is_valid_nonce,
    server_proof, verify_client_proof,
};
use fluxdown_protocol::{
    ApplicationErrorCode, RequestId, RpcErrorData, RpcErrorObject, RpcRequest, RpcResponse,
    ServiceRole, method, validate_first_request,
};
use tokio::task::{Id as TaskId, JoinSet};
use uuid::Uuid;

use crate::service::DaemonService;

/// 单条连接同时在途的慢调用上限；超出时立即以可重试的 `Unavailable` 拒绝。
pub const MAX_SLOW_CALLS: usize = 16;

/// 单次入站请求的响应及会话状态变化。
pub struct SessionReply {
    pub response: RpcResponse,
    pub became_ready: bool,
    /// 已受理 `system.shutdown`：传输层发出响应后应让整个 daemon 退出。
    pub shutdown_requested: bool,
    /// 发出响应后关闭连接（握手阶段的任何违规都不给对端第二次机会）。
    pub close: bool,
}

impl SessionReply {
    /// 不改变会话状态的普通响应。
    #[must_use]
    pub fn reply(response: RpcResponse) -> Self {
        Self {
            response,
            became_ready: false,
            shutdown_requested: false,
            close: false,
        }
    }

    fn closing(mut self) -> Self {
        self.close = true;
        self
    }
}

/// daemon 的认证状态：长期 token 与当前存活会话的 HTTP 凭据。
///
/// 新版 agent 只走挑战应答握手（token 不上线）并用会话派生的 HTTP 凭据；daemon 作为校验方
/// 同时继续接受持有 token 的旧式请求（升级头 / HTTP 的静态 Bearer）：包内二进制被替换后，
/// 仍常驻的旧版 agent 会重连到新 daemon，拒绝它只会让它彻底连不上，而「持有 token 的请求
/// 被接受」并不降低安全性——漏洞在客户端向未验证的对端发送 token。
pub struct DaemonAuth {
    token: Arc<str>,
    http_credentials: Mutex<HashMap<String, String>>,
}

impl DaemonAuth {
    #[must_use]
    pub fn new(token: impl Into<Arc<str>>) -> Self {
        Self {
            token: token.into(),
            http_credentials: Mutex::new(HashMap::new()),
        }
    }

    /// 旧式静态 Bearer 是否就是 daemon token。
    #[must_use]
    pub fn accepts_token(&self, presented: &str) -> bool {
        credentials_match(&self.token, presented)
    }

    /// HTTP 端点的 `Authorization: Bearer` 是否有效：某个存活会话派生的 HTTP 凭据，或旧式
    /// 静态 token。
    #[must_use]
    pub fn authorizes_http(&self, presented: &str) -> bool {
        let credentials = self
            .http_credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // 不短路：耗时与命中哪一条无关。
        let session = credentials.values().fold(false, |matched, credential| {
            credentials_match(credential, presented) | matched
        });
        session | self.accepts_token(presented)
    }

    /// 握手通过后登记本会话的 HTTP 凭据。
    pub(crate) fn register_http(&self, connection_id: &str, credential: String) {
        self.http_credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(connection_id.to_owned(), credential);
    }

    fn revoke_http(&self, connection_id: &str) {
        self.http_credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(connection_id);
    }
}

enum AuthStage {
    Anonymous,
    Challenged {
        client_nonce: String,
        server_nonce: String,
    },
    Authenticated,
}

/// 门禁放行结果。
enum GateStep {
    /// 握手帧或被拒绝的帧：直接回复。
    Reply(SessionReply),
    /// 已认证连接上的请求，交给会话继续处理。
    Authenticated(RpcRequest),
}

/// 单条连接的双向挑战应答握手：认证通过前只接受 `system.auth.challenge` /
/// `system.auth.prove`，其余任何帧（含 `system.hello`、`system.shutdown`）一律拒绝并断开。
struct AuthGate {
    auth: Arc<DaemonAuth>,
    connection_id: String,
    stage: AuthStage,
}

impl AuthGate {
    fn new(auth: Arc<DaemonAuth>, connection_id: String) -> Self {
        Self {
            auth,
            connection_id,
            stage: AuthStage::Anonymous,
        }
    }

    /// 升级头已带有效静态 Bearer 的连接（旧版 agent）：无需挑战应答。
    fn authenticated(auth: Arc<DaemonAuth>, connection_id: String) -> Self {
        Self {
            auth,
            connection_id,
            stage: AuthStage::Authenticated,
        }
    }

    fn is_authenticated(&self) -> bool {
        matches!(self.stage, AuthStage::Authenticated)
    }

    fn admit(&mut self, text: &str) -> GateStep {
        let authenticated = self.is_authenticated();
        let request = match serde_json::from_str::<RpcRequest>(text) {
            Ok(request) => request,
            Err(error) => {
                let reply = SessionReply::reply(RpcResponse::parse_failure(error.to_string()));
                return GateStep::Reply(if authenticated {
                    reply
                } else {
                    reply.closing()
                });
            }
        };
        if authenticated {
            if matches!(
                request.method.as_str(),
                SYSTEM_AUTH_CHALLENGE | SYSTEM_AUTH_PROVE
            ) {
                return GateStep::Reply(SessionReply::reply(RpcResponse::failure(
                    request.id,
                    RpcErrorObject::application(
                        "connection is already authenticated",
                        RpcErrorData::new(ApplicationErrorCode::Conflict, false),
                    ),
                )));
            }
            return GateStep::Authenticated(request);
        }
        GateStep::Reply(self.authenticate(request))
    }

    fn authenticate(&mut self, request: RpcRequest) -> SessionReply {
        if let Err(data) = request.validate() {
            return rejection(request.id, "invalid JSON-RPC version", data);
        }
        let stage = std::mem::replace(&mut self.stage, AuthStage::Anonymous);
        let is_challenge = request.method == SYSTEM_AUTH_CHALLENGE;
        let is_prove = request.method == SYSTEM_AUTH_PROVE;
        match stage {
            AuthStage::Anonymous if is_challenge => self.challenge(request),
            AuthStage::Challenged {
                client_nonce,
                server_nonce,
            } if is_prove => self.prove(request, &client_nonce, &server_nonce),
            _ => rejection(
                request.id,
                "authentication required",
                RpcErrorData::new(ApplicationErrorCode::Unauthorized, false),
            ),
        }
    }

    fn challenge(&mut self, request: RpcRequest) -> SessionReply {
        let params = request
            .params
            .and_then(|params| serde_json::from_value::<AuthChallengeParams>(params).ok())
            .filter(|params| is_valid_nonce(&params.client_nonce));
        let Some(params) = params else {
            return rejection(
                request.id,
                "invalid challenge",
                RpcErrorData {
                    field: Some("clientNonce".to_owned()),
                    ..RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
                },
            );
        };
        let server_nonce = fresh_nonce();
        let Some(proof) = server_proof(&self.auth.token, &params.client_nonce, &server_nonce)
        else {
            return rejection(
                request.id,
                "daemon token is unusable",
                RpcErrorData::new(ApplicationErrorCode::Internal, false),
            );
        };
        let result = AuthChallengeResult {
            server_nonce: server_nonce.clone(),
            server_proof: proof,
        };
        match serde_json::to_value(result) {
            Ok(result) => {
                self.stage = AuthStage::Challenged {
                    client_nonce: params.client_nonce,
                    server_nonce,
                };
                SessionReply::reply(RpcResponse::success(request.id, result))
            }
            Err(error) => rejection(
                request.id,
                error.to_string(),
                RpcErrorData::new(ApplicationErrorCode::Internal, false),
            ),
        }
    }

    fn prove(
        &mut self,
        request: RpcRequest,
        client_nonce: &str,
        server_nonce: &str,
    ) -> SessionReply {
        let proof = request
            .params
            .and_then(|params| serde_json::from_value::<AuthProveParams>(params).ok());
        let verified = proof.is_some_and(|proof| {
            verify_client_proof(
                &self.auth.token,
                client_nonce,
                server_nonce,
                &proof.client_proof,
            )
        });
        let credential = http_credential(&self.auth.token, client_nonce, server_nonce);
        let (true, Some(credential)) = (verified, credential) else {
            return rejection(
                request.id,
                "authentication failed",
                RpcErrorData::new(ApplicationErrorCode::Unauthorized, false),
            );
        };
        let Ok(result) = serde_json::to_value(AuthProveResult {
            authenticated: true,
        }) else {
            return rejection(
                request.id,
                "authentication result is not serializable",
                RpcErrorData::new(ApplicationErrorCode::Internal, false),
            );
        };
        self.auth.register_http(&self.connection_id, credential);
        self.stage = AuthStage::Authenticated;
        SessionReply::reply(RpcResponse::success(request.id, result))
    }
}

impl Drop for AuthGate {
    fn drop(&mut self) {
        self.auth.revoke_http(&self.connection_id);
    }
}

fn rejection(id: RequestId, message: impl Into<String>, data: RpcErrorData) -> SessionReply {
    SessionReply::reply(RpcResponse::failure(
        id,
        RpcErrorObject::application(message, data),
    ))
    .closing()
}

fn fresh_nonce() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// 一条已通过握手与门禁、待执行的普通调用。
pub struct PreparedCall {
    service: Arc<DaemonService>,
    connection_id: String,
    is_local_agent: bool,
    request: RpcRequest,
}

impl PreparedCall {
    #[must_use]
    pub fn id(&self) -> RequestId {
        self.request.id.clone()
    }

    #[must_use]
    pub fn is_slow(&self) -> bool {
        method::is_slow_daemon_method(&self.request.method)
    }

    pub async fn run(self) -> RpcResponse {
        self.service
            .call(&self.connection_id, self.is_local_agent, self.request)
            .await
    }
}

/// [`RpcSession::accept`] 的结果。
pub enum Step {
    /// 已同步得出的响应（握手、校验失败、`system.hello` / `system.shutdown`）。
    Reply(SessionReply),
    /// 普通调用；传输层按 [`PreparedCall::is_slow`] 选择 FIFO 在途槽或 [`SlowCalls`]。
    Call(PreparedCall),
}

/// 单条 WebSocket 连接的握手状态。
pub struct RpcSession {
    connection_id: String,
    ready: bool,
    is_local_agent: bool,
    gate: AuthGate,
    service: Arc<DaemonService>,
}

impl RpcSession {
    #[must_use]
    pub fn new(
        service: Arc<DaemonService>,
        auth: Arc<DaemonAuth>,
        bearer_authenticated: bool,
    ) -> Self {
        let connection_id = Uuid::new_v4().to_string();
        let gate = if bearer_authenticated {
            AuthGate::authenticated(auth, connection_id.clone())
        } else {
            AuthGate::new(auth, connection_id.clone())
        };
        Self {
            gate,
            connection_id,
            ready: false,
            is_local_agent: false,
            service,
        }
    }

    #[must_use]
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// 握手（`system.auth.*`）是否已完成；完成前连接只允许走握手帧。
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.gate.is_authenticated()
    }

    /// 解析并校验一条文本帧。认证前只接受握手帧；认证后首帧只能是兼容的
    /// `system.hello` 或 `system.shutdown`。
    pub fn accept(&mut self, text: &str) -> Step {
        let request = match self.gate.admit(text) {
            GateStep::Reply(reply) => return Step::Reply(reply),
            GateStep::Authenticated(request) => request,
        };
        // hello 前也受理：版本不兼容的新 agent 需要让旧 daemon 退出后再拉起同版本进程。
        if request.method == method::SYSTEM_SHUTDOWN && request.validate().is_ok() {
            return Step::Reply(SessionReply {
                response: RpcResponse::success(request.id, serde_json::json!({ "ok": true })),
                became_ready: false,
                shutdown_requested: true,
                close: false,
            });
        }
        if !self.ready {
            return Step::Reply(self.handle_hello(request));
        }
        let id = request.id.clone();
        if let Err(data) = request.validate() {
            return Step::Reply(SessionReply::reply(RpcResponse::failure(
                id,
                RpcErrorObject::application("invalid JSON-RPC version", data),
            )));
        }
        if request.method == method::SYSTEM_HELLO {
            return Step::Reply(SessionReply::reply(RpcResponse::failure(
                id,
                RpcErrorObject::application(
                    "system.hello is only valid as the first frame",
                    RpcErrorData::new(ApplicationErrorCode::Conflict, false),
                ),
            )));
        }
        Step::Call(PreparedCall {
            service: self.service.clone(),
            connection_id: self.connection_id.clone(),
            is_local_agent: self.is_local_agent,
            request,
        })
    }

    /// 连接断开时释放连接所有权选择订阅。
    pub fn disconnect(&self) {
        self.service.selections().unsubscribe(&self.connection_id);
    }

    fn handle_hello(&mut self, request: RpcRequest) -> SessionReply {
        let id = request.id.clone();
        match validate_first_request(&request, ServiceRole::Daemon) {
            Ok(hello) => match serde_json::to_value(self.service.hello()) {
                Ok(result) => {
                    self.ready = true;
                    self.is_local_agent = hello.client_name == "fluxdown-agent";
                    SessionReply {
                        response: RpcResponse::success(id, result),
                        became_ready: true,
                        shutdown_requested: false,
                        close: false,
                    }
                }
                Err(error) => SessionReply::reply(RpcResponse::failure(
                    id,
                    RpcErrorObject::application(
                        error.to_string(),
                        RpcErrorData::new(ApplicationErrorCode::Internal, false),
                    ),
                )),
            },
            Err(data) => SessionReply::reply(RpcResponse::failure(
                id,
                RpcErrorObject::application("hello rejected", data),
            )),
        }
    }
}

/// 一条连接上在途的慢调用：有界并发，完成的响应经 [`SlowCalls::next`] 取出，由 socket
/// 循环统一回写（写端只有循环一个，无需额外互斥）。丢弃即中止全部在途调用。
pub struct SlowCalls {
    tasks: JoinSet<RpcResponse>,
    ids: HashMap<TaskId, RequestId>,
    limit: usize,
}

impl SlowCalls {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            tasks: JoinSet::new(),
            ids: HashMap::new(),
            limit,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// 后台执行 `call`；在途数已达上限时返回应立即回给客户端的拒绝响应。
    pub fn spawn<F>(&mut self, id: RequestId, call: F) -> Result<(), Box<RpcResponse>>
    where
        F: Future<Output = RpcResponse> + Send + 'static,
    {
        if self.tasks.len() >= self.limit {
            return Err(Box::new(RpcResponse::failure(
                id,
                RpcErrorObject::application(
                    "too many concurrent long-running calls",
                    RpcErrorData::new(ApplicationErrorCode::Unavailable, true),
                ),
            )));
        }
        let handle = self.tasks.spawn(call);
        self.ids.insert(handle.id(), id);
        Ok(())
    }

    /// 等下一条完成的响应；没有在途调用时返回 `None`。取消安全。
    pub async fn next(&mut self) -> Option<RpcResponse> {
        loop {
            match self.tasks.join_next_with_id().await? {
                Ok((task_id, response)) => {
                    self.ids.remove(&task_id);
                    return Some(response);
                }
                Err(error) => {
                    // 调用 panic：客户端仍在等这个请求，给它一个内部错误而不是悬空。
                    if let Some(id) = self.ids.remove(&error.id()) {
                        return Some(RpcResponse::failure(
                            id,
                            RpcErrorObject::application(
                                "long-running call failed unexpectedly",
                                RpcErrorData::new(ApplicationErrorCode::Internal, false),
                            ),
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use fluxdown_protocol::handshake::{
        AuthChallengeResult, SYSTEM_AUTH_CHALLENGE, SYSTEM_AUTH_PROVE, client_proof,
        http_credential, verify_server_proof,
    };
    use fluxdown_protocol::{
        ApplicationErrorCode, RequestId, RpcErrorData, RpcRequest, RpcResponse, method,
    };
    use serde_json::{Value, json};

    use fluxdown_protocol::method::{
        SLOW_DAEMON_METHODS as SLOW_METHODS, is_slow_daemon_method as is_slow_method,
    };

    use super::{AuthGate, DaemonAuth, GateStep, MAX_SLOW_CALLS, SessionReply, SlowCalls};

    const TOKEN: &str = "daemon-rpc-gate-token";
    const CLIENT_NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn frame(id: i64, method_name: &str, params: Option<Value>) -> String {
        serde_json::to_string(&RpcRequest::new(
            RequestId::Integer(id),
            method_name,
            params,
        ))
        .expect("serialize frame")
    }

    fn gate(auth: &Arc<DaemonAuth>, connection_id: &str) -> AuthGate {
        AuthGate::new(auth.clone(), connection_id.to_owned())
    }

    fn reply(step: GateStep) -> SessionReply {
        match step {
            GateStep::Reply(reply) => reply,
            GateStep::Authenticated(request) => panic!("unexpected pass-through: {request:?}"),
        }
    }

    fn error_code(reply: &SessionReply) -> ApplicationErrorCode {
        let RpcResponse::Failure(failure) = &reply.response else {
            panic!("expected failure, got {:?}", reply.response);
        };
        failure
            .error
            .data
            .as_ref()
            .map(|data: &RpcErrorData| data.code)
            .expect("application error data")
    }

    /// 走完质询，返回服务端随机数与服务端证明（与 agent 一侧的校验输入一致）。
    fn challenge(gate: &mut AuthGate, client_nonce: &str) -> AuthChallengeResult {
        let reply = reply(gate.admit(&frame(
            1,
            SYSTEM_AUTH_CHALLENGE,
            Some(json!({ "clientNonce": client_nonce })),
        )));
        assert!(!reply.close);
        let RpcResponse::Success(success) = reply.response else {
            panic!("challenge must succeed");
        };
        serde_json::from_value(success.result).expect("challenge result")
    }

    fn prove(gate: &mut AuthGate, proof: &str) -> SessionReply {
        reply(gate.admit(&frame(
            2,
            SYSTEM_AUTH_PROVE,
            Some(json!({ "clientProof": proof })),
        )))
    }

    #[test]
    fn anonymous_connection_is_refused_everything_but_the_challenge() {
        let auth = Arc::new(DaemonAuth::new(TOKEN));
        for text in [
            frame(1, method::SYSTEM_HELLO, Some(json!({}))),
            frame(1, method::SYSTEM_SHUTDOWN, None),
            frame(1, method::DAEMON_TASK_LIST, None),
            frame(1, SYSTEM_AUTH_PROVE, Some(json!({ "clientProof": "x" }))),
            "not json".to_owned(),
        ] {
            let mut gate = gate(&auth, "anonymous");
            let reply = reply(gate.admit(&text));
            assert!(reply.close, "{text}");
            assert!(!reply.shutdown_requested, "{text}");
            assert!(!reply.became_ready, "{text}");
            assert!(!gate.is_authenticated(), "{text}");
        }
        let mut gate = gate(&auth, "anonymous");
        let refused = reply(gate.admit(&frame(1, method::SYSTEM_SHUTDOWN, None)));
        assert_eq!(error_code(&refused), ApplicationErrorCode::Unauthorized);
    }

    #[test]
    fn mutual_handshake_authenticates_and_issues_a_session_http_credential() {
        let auth = Arc::new(DaemonAuth::new(TOKEN));
        let mut session = gate(&auth, "session-a");
        let challenged = challenge(&mut session, CLIENT_NONCE);
        // agent 侧先验服务端证明，通过后才会提交自己的证明。
        assert!(verify_server_proof(
            TOKEN,
            CLIENT_NONCE,
            &challenged.server_nonce,
            &challenged.server_proof
        ));
        let proof = client_proof(TOKEN, CLIENT_NONCE, &challenged.server_nonce).expect("proof");
        let accepted = prove(&mut session, &proof);
        assert!(!accepted.close);
        assert!(session.is_authenticated());

        let credential =
            http_credential(TOKEN, CLIENT_NONCE, &challenged.server_nonce).expect("credential");
        assert!(auth.authorizes_http(&credential));
        // 随机串不是凭据。
        assert!(!auth.authorizes_http(""));
        assert!(!auth.authorizes_http("a-guess-of-the-credential"));

        // 认证后的普通请求放行；握手方法不能再来一次。
        let GateStep::Authenticated(request) =
            session.admit(&frame(3, method::SYSTEM_HELLO, Some(json!({}))))
        else {
            panic!("authenticated frame must pass through");
        };
        assert_eq!(request.method, method::SYSTEM_HELLO);
        let again = reply(session.admit(&frame(
            4,
            SYSTEM_AUTH_CHALLENGE,
            Some(json!({ "clientNonce": CLIENT_NONCE })),
        )));
        assert_eq!(error_code(&again), ApplicationErrorCode::Conflict);
        assert!(!again.close);
    }

    #[test]
    fn static_token_bearer_from_older_agents_is_still_accepted() {
        let auth = Arc::new(DaemonAuth::new(TOKEN));
        assert!(auth.accepts_token(TOKEN));
        assert!(!auth.accepts_token("wrong-token"));
        // 旧版 agent 对 /blobs、/files、/exports 发送的静态 Bearer。
        assert!(auth.authorizes_http(TOKEN));
        assert!(!auth.authorizes_http("wrong-token"));

        // 升级头已带有效 Bearer：首帧直接是 hello，不要求挑战应答。
        let mut legacy = AuthGate::authenticated(auth.clone(), "legacy".to_owned());
        assert!(legacy.is_authenticated());
        let GateStep::Authenticated(request) =
            legacy.admit(&frame(1, method::SYSTEM_HELLO, Some(json!({}))))
        else {
            panic!("bearer-authenticated connection must pass frames through");
        };
        assert_eq!(request.method, method::SYSTEM_HELLO);
        // 这条路径不登记会话凭据；静态 token 不属于任何会话，连接结束后依旧有效。
        drop(legacy);
        assert!(auth.authorizes_http(TOKEN));
    }

    #[test]
    fn proof_from_a_client_without_the_token_is_rejected() {
        let auth = Arc::new(DaemonAuth::new(TOKEN));
        let mut session = gate(&auth, "session-a");
        let challenged = challenge(&mut session, CLIENT_NONCE);
        let forged =
            client_proof("not-the-token", CLIENT_NONCE, &challenged.server_nonce).expect("proof");
        let rejected = prove(&mut session, &forged);
        assert!(rejected.close);
        assert_eq!(error_code(&rejected), ApplicationErrorCode::Unauthorized);
        assert!(!session.is_authenticated());
        let forged_credential =
            http_credential("not-the-token", CLIENT_NONCE, &challenged.server_nonce)
                .expect("credential");
        assert!(!auth.authorizes_http(&forged_credential));
    }

    #[test]
    fn recorded_proof_cannot_be_replayed_on_another_connection() {
        let auth = Arc::new(DaemonAuth::new(TOKEN));
        let mut first = gate(&auth, "session-a");
        let first_challenge = challenge(&mut first, CLIENT_NONCE);
        let recorded =
            client_proof(TOKEN, CLIENT_NONCE, &first_challenge.server_nonce).expect("proof");
        assert!(!prove(&mut first, &recorded).close);

        // 攻击者复用同一个 clientNonce 与录下的证明：服务端随机数每次连接都是新的。
        let mut second = gate(&auth, "session-b");
        let second_challenge = challenge(&mut second, CLIENT_NONCE);
        assert_ne!(first_challenge.server_nonce, second_challenge.server_nonce);
        let replayed = prove(&mut second, &recorded);
        assert!(replayed.close);
        assert_eq!(error_code(&replayed), ApplicationErrorCode::Unauthorized);
        assert!(!second.is_authenticated());
    }

    #[test]
    fn prove_without_challenge_and_malformed_challenges_are_rejected() {
        let auth = Arc::new(DaemonAuth::new(TOKEN));
        let mut skipped = gate(&auth, "skipped");
        let rejected = prove(&mut skipped, "deadbeef");
        assert!(rejected.close);
        assert!(!skipped.is_authenticated());

        for params in [
            None,
            Some(json!({})),
            Some(json!({ "clientNonce": "short" })),
            Some(json!({ "clientNonce": "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz" })),
            Some(json!({ "clientNonce": CLIENT_NONCE, "extra": 1 })),
        ] {
            let mut gate = gate(&auth, "malformed");
            let rejected = reply(gate.admit(&frame(1, SYSTEM_AUTH_CHALLENGE, params.clone())));
            assert!(rejected.close, "{params:?}");
            assert_eq!(
                error_code(&rejected),
                ApplicationErrorCode::InvalidArgument,
                "{params:?}"
            );
        }
    }

    #[test]
    fn http_credential_lives_exactly_as_long_as_its_session() {
        let auth = Arc::new(DaemonAuth::new(TOKEN));
        let mut first = gate(&auth, "session-a");
        let first_challenge = challenge(&mut first, CLIENT_NONCE);
        let proof =
            client_proof(TOKEN, CLIENT_NONCE, &first_challenge.server_nonce).expect("proof");
        assert!(!prove(&mut first, &proof).close);
        let first_credential = http_credential(TOKEN, CLIENT_NONCE, &first_challenge.server_nonce)
            .expect("credential");

        let other_nonce = "fedcba9876543210fedcba9876543210";
        let mut second = gate(&auth, "session-b");
        let second_challenge = challenge(&mut second, other_nonce);
        let proof =
            client_proof(TOKEN, other_nonce, &second_challenge.server_nonce).expect("proof");
        assert!(!prove(&mut second, &proof).close);
        let second_credential = http_credential(TOKEN, other_nonce, &second_challenge.server_nonce)
            .expect("credential");

        assert_ne!(first_credential, second_credential);
        assert!(auth.authorizes_http(&first_credential));
        assert!(auth.authorizes_http(&second_credential));
        drop(first);
        assert!(!auth.authorizes_http(&first_credential));
        assert!(auth.authorizes_http(&second_credential));
        drop(second);
        assert!(!auth.authorizes_http(&second_credential));
    }

    #[test]
    fn slow_method_list_is_canonical_and_keeps_ordering_sensitive_calls_inline() {
        let mut seen = std::collections::HashSet::new();
        for name in SLOW_METHODS {
            assert!(seen.insert(*name), "{name} listed twice");
            assert!(
                method::ALL_METHODS.contains(name),
                "{name} is not a canonical method"
            );
            assert!(name.starts_with("daemon."), "{name}");
            assert!(is_slow_method(name));
        }
        for name in [
            method::DAEMON_COMPONENT_INSTALL,
            method::DAEMON_COMPONENT_LIST_VERSIONS,
            method::DAEMON_PLUGIN_MARKET_LIST,
            method::DAEMON_PLUGIN_MARKET_INSTALL,
            method::DAEMON_PLUGIN_INSTALL,
            method::DAEMON_PLUGIN_AUTH,
            method::DAEMON_GROUP_RESOLVE_PREVIEW,
            method::DAEMON_RSS_VALIDATE,
            method::DAEMON_LINK_PROBE,
            method::DAEMON_CONFIG_PROXY_TEST,
            method::DAEMON_CONFIG_PROXY_ROUTE,
            method::DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH,
            method::DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH,
            method::DAEMON_ED2K_NODES_SUBSCRIPTION_REFRESH,
            method::DAEMON_ED2K_ID_CHECK,
        ] {
            assert!(
                is_slow_method(name),
                "{name} must not block the socket loop"
            );
        }
        // 握手、快照与改变任务 / 配置状态的调用必须保持同连接内的请求顺序。
        for name in [
            method::SYSTEM_HELLO,
            method::SYSTEM_SHUTDOWN,
            method::SYSTEM_PING,
            method::SYSTEM_SNAPSHOT,
            method::DAEMON_TASK_CREATE,
            method::DAEMON_TASK_PAUSE,
            method::DAEMON_TASK_DELETE,
            method::DAEMON_CONFIG_PATCH,
            method::DAEMON_SELECTION_SUBSCRIBE,
            method::DAEMON_SELECTION_RESOLVE,
        ] {
            assert!(!is_slow_method(name), "{name} must stay inline");
        }
    }

    fn success(id: i64) -> RpcResponse {
        RpcResponse::success(RequestId::Integer(id), json!({ "ok": true }))
    }

    fn response_id(response: &RpcResponse) -> Option<RequestId> {
        match response {
            RpcResponse::Success(success) => Some(success.id.clone()),
            RpcResponse::Failure(failure) => failure.id.clone(),
        }
    }

    #[tokio::test]
    async fn pending_slow_call_leaves_the_loop_free_and_responds_when_finished() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let mut slow = SlowCalls::new(MAX_SLOW_CALLS);
        assert!(slow.is_empty());
        slow.spawn(RequestId::Integer(7), async move {
            released.await.expect("release pending slow call");
            success(7)
        })
        .expect("spawn");
        assert!(!slow.is_empty());
        // 调用未完成时 `next` 保持挂起（socket 循环因此可以继续轮询事件与后续请求），
        // 反复取消也不会丢失结果。
        for _ in 0..3 {
            assert!(
                tokio::time::timeout(Duration::from_millis(20), slow.next())
                    .await
                    .is_err()
            );
        }
        release.send(()).expect("call is still waiting");
        let response = tokio::time::timeout(Duration::from_secs(2), slow.next())
            .await
            .expect("finished call is delivered")
            .expect("response");
        assert_eq!(response_id(&response), Some(RequestId::Integer(7)));
        assert!(slow.is_empty());
        assert!(slow.next().await.is_none());
    }

    #[tokio::test]
    async fn slow_call_concurrency_is_bounded_and_rejections_name_their_request() {
        let mut slow = SlowCalls::new(2);
        let mut releases = Vec::new();
        for id in 1..=2 {
            let (release, released) = tokio::sync::oneshot::channel::<()>();
            releases.push(release);
            slow.spawn(RequestId::Integer(id), async move {
                released.await.expect("release bounded slow call");
                success(id)
            })
            .expect("within limit");
        }
        let rejected = slow
            .spawn(RequestId::Integer(3), async { success(3) })
            .expect_err("limit reached");
        let RpcResponse::Failure(failure) = *rejected else {
            panic!("rejection must be a failure");
        };
        assert_eq!(failure.id, Some(RequestId::Integer(3)));
        let data = failure.error.data.expect("error data");
        assert_eq!(data.code, ApplicationErrorCode::Unavailable);
        assert!(data.retryable);

        for release in releases {
            release
                .send(())
                .expect("bounded slow call is still waiting");
        }
        let first = tokio::time::timeout(Duration::from_secs(2), slow.next())
            .await
            .expect("first")
            .expect("response");
        assert!(response_id(&first).is_some());
        // 有一个名额释放后可以再接受新的慢调用。
        slow.spawn(RequestId::Integer(4), async { success(4) })
            .expect("capacity freed");
    }

    #[tokio::test]
    async fn panicking_slow_call_answers_its_request_with_an_internal_error() {
        let mut slow = SlowCalls::new(MAX_SLOW_CALLS);
        slow.spawn(RequestId::Integer(9), async {
            panic!("slow call exploded")
        })
        .expect("spawn");
        let response = tokio::time::timeout(Duration::from_secs(2), slow.next())
            .await
            .expect("panic is reported")
            .expect("response");
        let RpcResponse::Failure(failure) = response else {
            panic!("panic must surface as a failure");
        };
        assert_eq!(failure.id, Some(RequestId::Integer(9)));
        assert_eq!(
            failure.error.data.map(|data| data.code),
            Some(ApplicationErrorCode::Internal)
        );
    }

    #[tokio::test]
    async fn dropping_the_connection_aborts_in_flight_slow_calls() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let aborted = Arc::new(AtomicBool::new(false));
        let mut slow = SlowCalls::new(MAX_SLOW_CALLS);
        let flag = DropFlag(aborted.clone());
        slow.spawn(RequestId::Integer(1), async move {
            let _flag = flag;
            std::future::pending::<()>().await;
            success(1)
        })
        .expect("spawn");
        tokio::task::yield_now().await;
        drop(slow);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !aborted.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("in-flight call is aborted with the connection");
    }
}
