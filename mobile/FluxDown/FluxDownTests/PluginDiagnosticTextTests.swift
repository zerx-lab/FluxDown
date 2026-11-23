import FluxDomain
import Foundation
import Testing
@testable import FluxDown

#if FULL_FLAVOR
@MainActor
struct PluginDiagnosticTextTests {
    @Test func scriptPositionUsesOnlyStructuredArguments() {
        let error = HostError(
            .invalidArgument, reason: "pluginPackageInvalid", message: "/private/example.fxplug",
            pluginDiagnostic: PluginDiagnostic(kind: "scriptCompileFailedAt", line: 12, column: 3)
        )

        let text = ExtensionErrorText.pluginFailure(error)
        #expect(text == L("pluginDiagnosticScriptCompileFailedAt", ["line": 12, "column": 3]))
        #expect(text.contains("12:3"))
        #expect(!text.contains(error.message))
    }

    @Test func unknownDiagnosticUsesTheSafeReasonFallback() {
        let error = HostError(
            .invalidArgument, reason: "pluginPackageInvalid", message: "raw daemon error",
            pluginDiagnostic: PluginDiagnostic(kind: "futureDiagnostic", path: "private-path")
        )

        let text = ExtensionErrorText.pluginFailure(error)
        #expect(text == L("pluginErrorPackageInvalid"))
        #expect(!text.contains(error.message))
        #expect(!text.contains("private-path"))
    }

    @Test(arguments: [
        (CocoaError.Code.fileReadNoPermission, "pluginImportFileNoPermission"),
        (.fileReadNoSuchFile, "mobileFileNotFound"),
        (.fileReadUnknown, "pluginImportFileUnreadable"),
    ])
    func packageImportFailureKeepsTheCategoryButNotThePath(_ code: CocoaError.Code, _ key: String) {
        let path = "/private/example.fxplug"
        let error = NSError(domain: NSCocoaErrorDomain, code: code.rawValue, userInfo: [
            NSFilePathErrorKey: path,
            NSLocalizedDescriptionKey: "Provider could not open \(path)",
        ])
        let message = ExtensionErrorText.packageImportFailure(error)
        #expect(message == L("pluginOpInstallFailed", ["message": L(key)]))
        #expect(message?.contains(path) == false)
    }

    @Test func cancellingPackageSelectionDoesNotShowAnError() {
        #expect(ExtensionErrorText.packageImportFailure(CocoaError(.userCancelled)) == nil)
    }
}
#endif
