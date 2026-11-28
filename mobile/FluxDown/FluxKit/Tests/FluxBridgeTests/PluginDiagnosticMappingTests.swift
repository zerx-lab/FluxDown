@testable import FluxBridge
import FluxDomain
import FluxRustBindings
import Testing

struct PluginDiagnosticMappingTests {
    @Test func rpcErrorCarriesScriptLocation() {
        let error = FluxError.Rpc(
            code: .invalidArgument, reason: "pluginPackageInvalid", retryable: false, detail: "",
            pluginDiagnostic: PluginDiagnosticDto(kind: "scriptCompileFailedAt", path: nil, line: 12, column: 3)
        )

        let mapped = hostError(error)
        #expect(mapped.code == .invalidArgument)
        #expect(mapped.reason == "pluginPackageInvalid")
        #expect(mapped.pluginDiagnostic == PluginDiagnostic(kind: "scriptCompileFailedAt", line: 12, column: 3))
    }

    @Test func hostErrorDtoCarriesRelativePackagePath() {
        let error = HostErrorDto(
            code: .invalidArgument, reason: nil, retryable: false, message: "",
            pluginDiagnostic: PluginDiagnosticDto(kind: "missingFile", path: "manifest.json", line: nil, column: nil)
        ).domain

        #expect(error.pluginDiagnostic == PluginDiagnostic(kind: "missingFile", path: "manifest.json"))
    }
}
