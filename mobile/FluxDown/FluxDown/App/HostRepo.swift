import FluxDomain
import Foundation
import Security

/// 已保存的远端主机（同 Android `HostRepo`）：`fluxdown-agent --server` / 开启局域网访问的桌面 agent。
///
/// 列表（id / 名称 / 地址 / 可选 `linkFingerprint`）存 `UserDefaults`；凭据（访问密钥，或局域网配对得到的设备控制凭据）
/// 存钥匙串（`kSecClassGenericPassword`，`AfterFirstUnlockThisDeviceOnly`：不随备份迁移到新设备，换机后需重新输入 / 重新配对）。
/// `linkFingerprint` 非空 = 该主机是经局域网配对添加的（凭据是设备控制凭据，指纹是主机的链路身份指纹）；旧数据缺省为 nil。
@MainActor
final class HostRepo {
    private struct Saved: Codable {
        var id: String
        var name: String
        var endpoint: String
        var linkFingerprint: String?
    }

    private static let listKey = "hosts.remotes"
    private static let lastRemoteKey = "hosts.lastRemote"
    private static let service = "com.fluxdown.app.host-access-key"

    private let defaults: UserDefaults
    private(set) var remotes: [HostRef] = []
    private var fingerprints: [String: String] = [:]

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        publish(load())
    }

    var lastRemote: HostRef? {
        guard let id = defaults.string(forKey: Self.lastRemoteKey) else { return nil }
        return remotes.first { $0.id == id }
    }

    func rememberRemote(_ id: String?) {
        defaults.set(id, forKey: Self.lastRemoteKey)
    }

    private func load() -> [Saved] {
        guard let data = defaults.data(forKey: Self.listKey) else { return [] }
        return (try? JSONDecoder().decode([Saved].self, from: data)) ?? []
    }

    private func publish(_ list: [Saved]) {
        remotes = list.map { .remote(id: $0.id, displayName: $0.name, endpoint: $0.endpoint) }
        fingerprints = Dictionary(
            list.compactMap { saved in saved.linkFingerprint.map { (saved.id, $0) } },
            uniquingKeysWith: { first, _ in first }
        )
    }

    private func store(_ list: [Saved]) throws(HostError) {
        do {
            defaults.set(try JSONEncoder().encode(list), forKey: Self.listKey)
        } catch {
            throw HostError(.internal, message: "encode hosts: \(error)")
        }
        publish(list)
    }

    /// 该主机经局域网配对添加时的主机链路指纹；访问密钥主机 = nil。
    func linkFingerprint(id: String) -> String? { fingerprints[id] }

    /// 已保存的、对应同一链路指纹的主机（重新配对时更新它而不是新增一台）。
    func remote(linkFingerprint: String) -> HostRef? {
        guard let id = fingerprints.first(where: { $0.value == linkFingerprint })?.key else { return nil }
        return remotes.first { $0.id == id }
    }

    func add(name: String, endpoint: String, accessKey: String, linkFingerprint: String? = nil) throws(HostError) -> HostRef {
        let id = UUID().uuidString.lowercased()
        try Keychain.set(accessKey, service: Self.service, account: id)
        var list = load()
        list.append(Saved(id: id, name: name, endpoint: endpoint, linkFingerprint: linkFingerprint))
        try store(list)
        return .remote(id: id, displayName: name, endpoint: endpoint)
    }

    /// 重新配对：同一主机换新凭据与最新地址（名称沿用用户已有的）。
    func updateCredential(id: String, endpoint: String, accessKey: String) throws(HostError) -> HostRef {
        var list = load()
        guard let index = list.firstIndex(where: { $0.id == id }) else {
            throw HostError(.notFound, message: "host not found")
        }
        try Keychain.set(accessKey, service: Self.service, account: id)
        list[index].endpoint = endpoint
        try store(list)
        return .remote(id: id, displayName: list[index].name, endpoint: endpoint)
    }

    func remove(id: String) throws(HostError) {
        try store(load().filter { $0.id != id })
        if defaults.string(forKey: Self.lastRemoteKey) == id { rememberRemote(nil) }
        try Keychain.delete(service: Self.service, account: id)
    }

    func accessKey(id: String) throws(HostError) -> String? {
        try Keychain.get(service: Self.service, account: id)
    }

    /// 远端主机访问密钥轮换（API 服务页改密钥后）。
    func updateAccessKey(id: String, key: String) throws(HostError) {
        try Keychain.set(key, service: Self.service, account: id)
    }
}

/// 钥匙串通用密码项的最小封装；失败一律转成 `HostError`，不吞错。
enum Keychain {
    private static func query(service: String, account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    static func set(_ secret: String, service: String, account: String) throws(HostError) {
        let base = query(service: service, account: account)
        let deleted = SecItemDelete(base as CFDictionary)
        guard deleted == errSecSuccess || deleted == errSecItemNotFound else {
            throw HostError(.internal, message: "keychain delete failed: \(deleted)")
        }
        var item = base
        item[kSecValueData as String] = Data(secret.utf8)
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(item as CFDictionary, nil)
        guard status == errSecSuccess else { throw HostError(.internal, message: "keychain add failed: \(status)") }
    }

    static func get(service: String, account: String) throws(HostError) -> String? {
        var item = query(service: service, account: account)
        item[kSecReturnData as String] = true
        item[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(item as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw HostError(.internal, message: "keychain read failed: \(status)")
        }
        return String(decoding: data, as: UTF8.self)
    }

    static func delete(service: String, account: String) throws(HostError) {
        let status = SecItemDelete(query(service: service, account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw HostError(.internal, message: "keychain delete failed: \(status)")
        }
    }
}
