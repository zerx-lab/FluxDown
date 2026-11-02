import FluxDomain
import Foundation
import Testing

/// 通用通道的 Swift 侧约定：JSON 编解码、宽松枚举、类型化 `call` 扩展、方法 / 分区常量。
struct ProtocolChannelTests {
    // MARK: JSONValue / 宽松枚举

    @Test func jsonValueRoundTripsEveryShapeWithoutLosingIntegerPrecision() throws {
        let text = #"{"a":null,"b":true,"c":-3,"d":18446744073709551615,"e":1.5,"f":"x","g":[1,"y"],"h":{"k":false}}"#
        let value = try ProtocolJSON.decode(JSONValue.self, from: Data(text.utf8), what: "test")
        #expect(value["a"] == .null)
        #expect(value["b"]?.boolValue == true)
        #expect(value["c"]?.intValue == -3)
        #expect(value["d"] == .uint(UInt64.max))
        #expect(value["e"]?.doubleValue == 1.5)
        #expect(value["f"]?.stringValue == "x")
        #expect(value["g"]?.arrayValue?.count == 2)
        #expect(value["h"]?["k"] == .bool(false))
        let again = try ProtocolJSON.decode(JSONValue.self, from: try ProtocolJSON.encode(value, what: "test"), what: "test")
        #expect(again == value)
    }

    private enum PathStyle: WireStringEnum {
        case windows, posix
        case unknown(String)

        init(wire: String) {
            switch wire {
            case "windows": self = .windows
            case "posix": self = .posix
            default: self = .unknown(wire)
            }
        }

        var wire: String {
            switch self {
            case .windows: "windows"
            case .posix: "posix"
            case let .unknown(raw): raw
            }
        }
    }

    private struct Device: Codable, Equatable {
        var id: String
        var platform: String?
        var pathStyle: PathStyle?
    }

    @Test func unknownEnumValuesDecodeLeniently_optionalsMayBeMissing_andRoundTripLosslessly() throws {
        let data = Data(#"[{"id":"a","pathStyle":"plan9"},{"id":"b","platform":null,"pathStyle":"posix"}]"#.utf8)
        let devices = try ProtocolJSON.decode([Device].self, from: data, what: "devices")
        #expect(devices[0].pathStyle == .unknown("plan9"))
        #expect(devices[0].platform == nil)
        #expect(devices[1].pathStyle == .posix)
        let encoded = try ProtocolJSON.encode(devices, what: "devices")
        #expect(try ProtocolJSON.decode([Device].self, from: encoded, what: "devices") == devices)
        #expect(String(decoding: encoded, as: UTF8.self).contains("\"plan9\""))
    }

    @Test func decodeFailureMapsToInternalHostErrorNamingTheSource() {
        do {
            _ = try ProtocolJSON.decode(Device.self, from: Data(#"{"platform":"x"}"#.utf8), what: "daemon.x result")
            Issue.record("a missing required key must fail")
        } catch {
            #expect(error.code == .internal)
            #expect(error.message.contains("daemon.x result"))
            #expect(error.message.contains("id"))
        }
    }

    // MARK: 类型化 call 扩展

    private struct Params: Encodable { var taskId: String; var deleteFiles: Bool }
    private struct Created: Decodable, Equatable { var taskId: String }

    private final class Recording: HostSession, @unchecked Sendable {
        let signals = AsyncStream<HostSignal> { $0.finish() }
        private let lock = NSLock()
        private var recorded: [(String, String?)] = []
        var reply = Data(#"{"taskId":"t-1"}"#.utf8)

        var calls: [(String, String?)] {
            lock.lock()
            defer { lock.unlock() }
            return recorded
        }

        func call(_ method: String, params: Data?) async throws(HostError) -> Data {
            let entry = (method, params.map { String(decoding: $0, as: UTF8.self) })
            lock.withLock { recorded.append(entry) }
            return reply
        }

        func close() {}
        func categorySaveDir(fileName _: String, url _: String) -> String? { nil }
        func createTask(_: CreateTaskRequest) async throws(HostError) -> String { "" }
        func pause(_: String) async throws(HostError) {}
        func resume(_: String) async throws(HostError) {}
        func delete(_: String, deleteFiles _: Bool) async throws(HostError) {}
        func pauseMany(_: [String]) async throws(HostError) {}
        func resumeMany(_: [String]) async throws(HostError) {}
        func deleteMany(_: [String], deleteFiles _: Bool) async throws(HostError) {}
        func pauseAll() async throws(HostError) {}
        func resumeAll() async throws(HostError) {}
        func rename(_: String, fileName _: String) async throws(HostError) {}
        func changeUrl(_: String, url _: String) async throws(HostError) {}
        func rescan() async throws(HostError) {}
        func moveToQueue(_: String, queueId _: String) async throws(HostError) {}
        func boost(_: String) async throws(HostError) {}
        func resolveSelection(_: String, outcome _: SelectionOutcome) async throws(HostError) {}
        func patchConfig(expectedRevision _: UInt64, values _: [String: String]) async throws(HostError) {}
        func refreshRssSource(_: String) async throws(HostError) {}
        func setRssSourceEnabled(_: String, enabled _: Bool) async throws(HostError) {}
    }

    @Test func typedCallEncodesCamelCaseParamsAndDecodesTheResult() async throws {
        let session = Recording()
        let created: Created = try await session.call(
            HostMethod.daemonTaskDelete, params: Params(taskId: "t", deleteFiles: true)
        )
        #expect(created == Created(taskId: "t-1"))
        let none: Created = try await session.call(HostMethod.daemonTaskList)
        #expect(none == created)
        try await session.callVoid(HostMethod.daemonTaskPauseAll)
        try await session.callVoid(HostMethod.daemonTaskPause, params: Params(taskId: "t", deleteFiles: false))

        let calls = session.calls
        #expect(calls.map(\.0) == ["daemon.task.delete", "daemon.task.list", "daemon.task.pauseAll", "daemon.task.pause"])
        #expect(calls[0].1 == #"{"deleteFiles":true,"taskId":"t"}"#)
        #expect(calls[1].1 == nil)
        #expect(calls[2].1 == nil)
    }

    @Test func typedCallReportsUndecodableResultsAsInternalErrors() async {
        let session = Recording()
        session.reply = Data(#"{"unexpected":1}"#.utf8)
        do {
            let _: Created = try await session.call(HostMethod.daemonTaskCreate)
            Issue.record("a result of the wrong shape must fail")
        } catch {
            #expect(error.code == .internal)
            #expect(error.message.contains("daemon.task.create"))
        }
    }

    // MARK: 常量表

    @Test func methodAndSectionConstantsMatchTheWireNames() {
        #expect(HostMethod.daemonTaskChangeUrl == "daemon.task.changeUrl")
        #expect(HostMethod.daemonRssListSources == "daemon.rss.listSources")
        #expect(HostMethod.agentLinkDiscoverySet == "agent.link.discovery.set")
        #expect(HostMethod.agentPreferencesPatch == "agent.preferences.patch")
        #expect(HostMethod.serviceEvent == "service.event")
        #expect(HostMethod.agentCloudNotifyTelegramBindStatus == "agent.cloudNotify.telegramBindStatus")
        #expect(HostMethod.agentCloudNotifySendEmailCode == "agent.cloudNotify.sendEmailCode")
        #expect(HostMethod.agentCloudNotifyVerifyEmail == "agent.cloudNotify.verifyEmail")
        #expect(HostMethod.agentProfileSendDeleteCode == "agent.profile.sendDeleteCode")
        #expect(HostMethod.agentProfileDeleteAccount == "agent.profile.deleteAccount")
        #expect(HostSection.agentCloudNotify == "agent.cloudNotify")
        #expect(HostCapability.agentRemoteTasks == "agent.remoteTasks")
        #expect(HostSection.all.count == 16)
        #expect(Set(HostSection.all).count == HostSection.all.count)
        #expect(HostSection.all.allSatisfy { $0.hasPrefix("agent.") || $0.hasPrefix("daemon.") })
    }
}
