@testable import FluxDomain
import Testing

struct SaveDirCategoryTests {
    private func category(_ id: String, dir: String, position: Int64, builtin: String? = nil, visible: Bool = true) -> CustomCategoryDto {
        CustomCategoryDto(
            id: id, name: id, position: position, visible: visible,
            isBuiltin: builtin != nil, builtinType: builtin, saveDir: dir
        )
    }

    private var list: [CustomCategoryDto] {
        [
            category("all", dir: "/all", position: 0, builtin: "all"),
            category("docs", dir: "/docs", position: 2),
            category("program", dir: "/Programs", position: 1, builtin: "program"),
            category("empty", dir: "  ", position: 3),
            category("hidden", dir: "/hidden", position: 4, visible: false),
            category("other", dir: "/other", position: 5, builtin: "other"),
        ]
    }

    @Test func optionsKeepVisibleNonAllCategoriesWithDirsInPositionOrder() {
        let options = SaveDirCategoryState.options(list)
        #expect(options.map(\.id) == ["program", "docs", "other"])
        #expect(SaveDirCategoryState.options(CustomCategoryDto.builtinDefaults).isEmpty)
    }

    @Test func matchedIsFirstOptionWhoseDirEqualsThePrediction() {
        let options = SaveDirCategoryState.options(list)
        #expect(SaveDirCategoryState.matched(options, predicted: " /Programs ")?.id == "program")
        #expect(SaveDirCategoryState.matched(options, predicted: "/elsewhere") == nil)
        #expect(SaveDirCategoryState.matched(options, predicted: nil) == nil)
    }

    @Test func predictionNamePrefersRenameOverRecognizedName() {
        #expect(SaveDirCategoryState.predictionName(rename: " a.exe ", entryFileName: "b.zip") == "a.exe")
        #expect(SaveDirCategoryState.predictionName(rename: "  ", entryFileName: "b.zip") == "b.zip")
        #expect(SaveDirCategoryState.predictionName(rename: "", entryFileName: "") == "")
    }

    @Test func autoFollowsPredictionUntilTheUserEditsTheDirectory() {
        var state = SaveDirCategoryState(initialDir: "/default")
        // 初始目录 / 空目录 / 上次自动写入值都继续跟随。
        #expect(state.follow(dir: "/default", offered: "/default", predicted: "/Programs") == "/Programs")
        #expect(state.follow(dir: "/Programs", offered: "/default", predicted: "/Videos") == "/Videos")
        #expect(state.follow(dir: "", offered: "/default", predicted: nil) == "/default")
        // 预测消失回到初始目录。
        #expect(state.follow(dir: "/Videos", offered: "/default", predicted: nil) == nil)
        #expect(state.lastWritten == "/default")
        // 用户手动改过：不再覆盖。
        #expect(state.follow(dir: "/mine", offered: "/default", predicted: "/Programs") == nil)
    }
    @Test func explicitlyEditedDirectoryDisablesRoutingEvenWhenItEqualsDefaultOrCategoryDir() {
        let options = SaveDirCategoryState.options(list)
        for dir in ["/default", "/Programs", "/mine"] {
            var state = SaveDirCategoryState(initialDir: "/default")
            state.markSaveDirManual()
            #expect(state.follow(dir: dir, offered: "/default", predicted: "/Videos") == nil)
            #expect(state.offeredSaveDir(dir: dir, initial: "/default") == nil)
        }

        var category = SaveDirCategoryState(initialDir: "/default")
        category.markSaveDirManual()
        #expect(category.select(.auto, offered: "/default", options: options, predicted: "/Programs") == "/Programs")
        #expect(category.offeredSaveDir(dir: "/Programs", initial: "/default") == "/default")
    }

    @Test func openingWithAPrefilledCategoryDirCountsAsAutoWritten() {
        var state = SaveDirCategoryState(initialDir: "/Programs")
        #expect(state.follow(dir: "/Programs", offered: "/default", predicted: "/Videos") == "/Videos")
    }

    @Test func noCategoryAndCategoryModesNeverFollow() {
        let options = SaveDirCategoryState.options(list)
        var state = SaveDirCategoryState(initialDir: "/default")
        #expect(state.select(.noCategory, offered: "/default", options: options, predicted: "/Programs") == "/default")
        #expect(state.follow(dir: "/default", offered: "/default", predicted: "/Programs") == nil)
        #expect(state.offeredSaveDir(dir: "/default", initial: "/default") == nil)

        // 显式选择分类：目录取分类目录，不论预测。
        #expect(state.select(.category("docs"), offered: "/default", options: options, predicted: "/Programs") == "/docs")
        #expect(state.follow(dir: "/docs", offered: "/default", predicted: "/Programs") == nil)
        #expect(state.offeredSaveDir(dir: "/docs", initial: "/default") == nil)

        // 已不存在的分类：不切换。
        #expect(state.select(.category("gone"), offered: "/default", options: options, predicted: nil) == nil)
        #expect(state.mode == .category("docs"))
    }

    @Test func switchingBackToAutoRecomputesImmediately() {
        let options = SaveDirCategoryState.options(list)
        var state = SaveDirCategoryState(initialDir: "/default")
        _ = state.select(.category("docs"), offered: "/default", options: options, predicted: nil)
        #expect(state.select(.auto, offered: "/default", options: options, predicted: "/Programs") == "/Programs")
        #expect(state.offeredSaveDir(dir: "/Programs", initial: "/default") == "/default")
        #expect(state.follow(dir: "/Programs", offered: "/default", predicted: "/other") == "/other")
        #expect(state.select(.auto, offered: "/default", options: options, predicted: nil) == "/default")
    }

    /// 回归：自动命中分类目录后，用户把目录手动改回默认目录，不再被覆盖，提交也不路由。
    @Test func manualEditBackToOfferedIsNotOverwrittenAndNotRouted() {
        var state = SaveDirCategoryState(initialDir: "/default")
        #expect(state.follow(dir: "/default", offered: "/default", predicted: "/Programs") == "/Programs")
        #expect(state.offeredSaveDir(dir: "/Programs", initial: "/default") == "/default")
        // 用户改回默认目录：lastWritten 已存在，等于 offered 也算手改。
        #expect(state.follow(dir: "/default", offered: "/default", predicted: "/Videos") == nil)
        #expect(state.offeredSaveDir(dir: "/default", initial: "/default") == nil)
        // 改成另一个分类目录同理。
        #expect(state.follow(dir: "/docs", offered: "/default", predicted: "/Videos") == nil)
        #expect(state.offeredSaveDir(dir: "/docs", initial: "/default") == nil)
        // 目录清空仍视为未手改。
        #expect(state.follow(dir: " ", offered: "/default", predicted: "/Videos") == "/Videos")
    }
}
