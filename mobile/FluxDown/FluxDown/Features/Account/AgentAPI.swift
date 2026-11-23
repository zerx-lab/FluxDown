import FluxDomain
import Foundation

/// 账户 / 同步 / 云端设备 / 远程任务 / 局域网直连的类型化 agent 调用：对 `HostSession.call` 的薄封装
/// （方法名见 `HostMethod`，DTO 见 `FluxDomain/Protocol/{Account,Sync,Devices,Link,RemoteTasks}.swift`）。
///
/// 每次调用都取当前会话快照（`AppContainer.agent`），因此切换主机后的新调用自然落到新主机。
/// 所有调用失败抛 `HostError`；文案用 ``AccountText/error(_:context:)``。
struct AgentAPI: Sendable {
    let session: any HostSession

    // MARK: 认证

    func register(_ params: RegisterParams) async throws(HostError) -> AgentLoginResult {
        try await session.call(HostMethod.agentAuthRegister, params: params)
    }

    func registerVerify(_ params: RegisterVerifyParams) async throws(HostError) -> AgentLoginResult {
        try await session.call(HostMethod.agentAuthRegisterVerify, params: params)
    }

    func login(_ params: LoginParams) async throws(HostError) -> AgentLoginResult {
        try await session.call(HostMethod.agentAuthLogin, params: params)
    }

    func loginVerify(_ params: LoginVerifyParams) async throws(HostError) -> AgentLoginResult {
        try await session.call(HostMethod.agentAuthLoginVerify, params: params)
    }

    func sendCode(email: String) async throws(HostError) -> TtlResult {
        try await session.call(HostMethod.agentAuthSendCode, params: SendCodeParams(email: email))
    }

    func verifyCode(_ params: VerifyCodeParams) async throws(HostError) -> AgentLoginResult {
        try await session.call(HostMethod.agentAuthVerifyCode, params: params)
    }

    func logout() async throws(HostError) {
        try await session.callVoid(HostMethod.agentAuthLogout)
    }

    /// 刷新云端资料（结果经 `agent.session` 分区推送，这里不解码返回值）。
    func refreshProfile() async throws(HostError) {
        try await session.callVoid(HostMethod.agentAuthRefreshProfile)
    }

    func sendPasswordResetCode(email: String) async throws(HostError) -> TtlResult {
        try await session.call(HostMethod.agentAuthSendPasswordResetCode, params: SendCodeParams(email: email))
    }

    func resetPassword(_ params: ResetPasswordParams) async throws(HostError) {
        try await session.callVoid(HostMethod.agentAuthResetPassword, params: params)
    }

    // MARK: 资料

    func sendEmailCode() async throws(HostError) -> TtlResult {
        try await session.call(HostMethod.agentProfileSendEmailCode)
    }

    func sendNewEmailCode(_ params: SendNewEmailCodeParams) async throws(HostError) -> TtlResult {
        try await session.call(HostMethod.agentProfileSendNewEmailCode, params: params)
    }

    func changeEmail(_ params: ChangeEmailParams) async throws(HostError) {
        try await session.callVoid(HostMethod.agentProfileChangeEmail, params: params)
    }

    func randomOriginId() async throws(HostError) -> RandomOriginIdResult {
        try await session.call(HostMethod.agentProfileRandomOriginId)
    }

    func checkOriginId(_ value: Int64) async throws(HostError) -> OriginIdCheckResult {
        try await session.call(HostMethod.agentProfileCheckOriginId, params: CheckOriginIdParams(value: value))
    }

    func changeOriginId(_ originId: Int64) async throws(HostError) {
        try await session.callVoid(HostMethod.agentProfileChangeOriginId, params: ChangeOriginIdParams(originId: originId))
    }

    func changeNickname(_ nickname: String) async throws(HostError) {
        try await session.callVoid(HostMethod.agentProfileChangeNickname, params: ChangeNicknameParams(nickname: nickname))
    }

    func sendPasswordCode() async throws(HostError) -> TtlResult {
        try await session.call(HostMethod.agentProfileSendPasswordCode)
    }

    func changePassword(_ params: ChangePasswordParams) async throws(HostError) {
        try await session.callVoid(HostMethod.agentProfileChangePassword, params: params)
    }

    func sendDeleteCode() async throws(HostError) -> TtlResult {
        try await session.call(HostMethod.agentProfileSendDeleteCode)
    }

    func deleteAccount(_ params: DeleteAccountParams) async throws(HostError) {
        try await session.callVoid(HostMethod.agentProfileDeleteAccount, params: params)
    }

    // MARK: 服务地址（仅调试 / TestFlight 构建 `editable == true`）

    func cloudEndpoint() async throws(HostError) -> CloudEndpointDto {
        try await session.call(HostMethod.agentCloudEndpointGet)
    }

    /// `baseUrl` 为空 = 恢复默认地址。
    func setCloudEndpoint(baseUrl: String) async throws(HostError) {
        try await session.callVoid(HostMethod.agentCloudEndpointSet, params: CloudEndpointSetParams(baseUrl: baseUrl))
    }

    // MARK: 配置同步

    func syncEnable() async throws(HostError) {
        try await session.callVoid(HostMethod.agentSyncEnable)
    }

    func syncDisable() async throws(HostError) {
        try await session.callVoid(HostMethod.agentSyncDisable)
    }

    func syncNow() async throws(HostError) {
        try await session.callVoid(HostMethod.agentSyncNow)
    }

    func syncSetLocalOnly(_ params: SyncLocalOnlyParams) async throws(HostError) {
        try await session.callVoid(HostMethod.agentSyncSetLocalOnly, params: params)
    }

    // MARK: 云端设备

    /// 同时刷新快照里的 `cloudDevices`。
    func deviceList() async throws(HostError) -> [CloudDeviceRecord] {
        let list: CloudDeviceList = try await session.call(HostMethod.agentDeviceList)
        return list.devices
    }

    func deviceRename(id: String, name: String) async throws(HostError) {
        try await session.callVoid(HostMethod.agentDeviceRename, params: DeviceRenameParams(id: id, name: name))
    }

    /// 删除当前设备同时清除本机会话。
    func deviceDelete(id: String) async throws(HostError) {
        try await session.callVoid(HostMethod.agentDeviceDelete, params: DeviceIdParams(id: id))
    }

    /// 请求立即重连云端任务连接；`accepted` 不表示已连上。
    func remoteReconnect() async throws(HostError) {
        try await session.callVoid(HostMethod.agentRemoteReconnect)
    }

    // MARK: 远程任务

    func remoteList() async throws(HostError) -> [RemoteTaskDto] {
        try await session.call(HostMethod.agentRemoteList)
    }

    func remoteDispatch(_ params: RemoteDispatchParams) async throws(HostError) -> RemoteTaskDto {
        let result: RemoteDispatchResult = try await session.call(HostMethod.agentRemoteDispatch, params: params)
        return result.task
    }

    func remoteCommand(_ params: RemoteCommandParams) async throws(HostError) {
        try await session.callVoid(HostMethod.agentRemoteCommand, params: params)
    }

    /// 批量下发同一动作（主机需声明 `agent.remoteBatch`；`params.taskIds` ≤ `RemoteCommandManyParams.maxIds`）。
    func remoteCommandMany(_ params: RemoteCommandManyParams) async throws(HostError) -> RemoteCommandManyResult {
        try await session.call(HostMethod.agentRemoteCommandMany, params: params)
    }

    // MARK: 局域网直连

    func linkPairingCode() async throws(HostError) -> LinkPairingCodeDto {
        try await session.call(HostMethod.agentLinkPairingCode)
    }

    /// 停止展示配对码；带 `code` 时只有它仍是当前码才作废（多客户端互不干扰），省略 = 无条件停止。
    func linkStopPairing(code: String? = nil) async throws(HostError) {
        if let code {
            try await session.callVoid(HostMethod.agentLinkStopPairing, params: LinkStopPairingParams(code: code))
        } else {
            try await session.callVoid(HostMethod.agentLinkStopPairing)
        }
    }

    func linkDiscoverySet(enabled: Bool) async throws(HostError) {
        try await session.callVoid(HostMethod.agentLinkDiscoverySet, params: LinkDiscoveryParams(enabled: enabled))
    }

    func linkProbe(address: String) async throws(HostError) -> LinkDiscoveredPeer {
        try await session.call(HostMethod.agentLinkProbe, params: LinkAddressParams(address: address))
    }

    func linkPairBegin(_ params: LinkPairBeginParams) async throws(HostError) -> LinkPairBeginResponse {
        try await session.call(HostMethod.agentLinkPairBegin, params: params)
    }

    /// 对端有 60 秒确认窗口；宿主调用超时按协议常量处理，调用方显示“等待对方确认”。
    func linkPairFinish(token: String, accept: Bool) async throws(HostError) -> LinkPairFinishResponse {
        try await session.call(HostMethod.agentLinkPairFinish, params: LinkPairFinishParams(token: token, accept: accept))
    }

    func linkApprove(sessionId: String, accept: Bool) async throws(HostError) {
        try await session.callVoid(HostMethod.agentLinkApprove, params: LinkApproveParams(sessionId: sessionId, accept: accept))
    }

    func linkRemove(fingerprint: String) async throws(HostError) {
        try await session.callVoid(HostMethod.agentLinkRemove, params: LinkDeviceParams(fingerprint: fingerprint))
    }

    /// 向（本机）agent 申请对已配对主机 `fingerprint` 的设备控制凭据；未以「请求完全控制」配对 / 已撤销时抛 `PeerNotPaired`。
    func linkControlCredential(fingerprint: String) async throws(HostError) -> LinkControlCredentialDto {
        try await session.call(HostMethod.agentLinkControlCredential, params: LinkDeviceParams(fingerprint: fingerprint))
    }

    /// 设置 / 清除本机备注名（`name` 空白 = 清除）。
    func linkRename(fingerprint: String, name: String?) async throws(HostError) -> LinkDeviceInfo {
        try await session.call(HostMethod.agentLinkRename, params: LinkRenameParams(fingerprint: fingerprint, name: name))
    }

    /// 探测全部已配对设备在线状态（同时推送快照里的 `linkDevices`）。
    func linkRefresh() async throws(HostError) -> [LinkDeviceInfo] {
        try await session.call(HostMethod.agentLinkRefresh)
    }

    func linkDispatch(_ params: LinkDispatchParams) async throws(HostError) -> LinkDispatchResult {
        try await session.call(HostMethod.agentLinkDispatch, params: params)
    }
}

extension AppContainer {
    /// 当前主机会话上的 agent 类型化调用。
    var agent: AgentAPI { AgentAPI(session: session) }
}
