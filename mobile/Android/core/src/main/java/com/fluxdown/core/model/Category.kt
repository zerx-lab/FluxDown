package com.fluxdown.core.model

/**
 * 自定义分类（`CustomCategoryDto`，偏好键 `custom_categories`）。
 * 列表由主机快照下发（Rust `CustomCategoryDto::from_preference`：空 / 损坏回退内置基线），
 * Kotlin 只做匹配，不另维护分类规则来源。
 */
data class Category(
    val id: String,
    /** 内置分类为空串：显示名走 i18n（`categoryVideo` …）。 */
    val name: String,
    val icon: String,
    val extensions: List<String>,
    /** 非空 = 正则模式（`matchMode = regex`）。 */
    val regexPattern: String = "",
    /** 非空 = 网址模式（`matchMode = url`）的通配符模式（桥接层只在 url 模式下下发）。 */
    val urlPattern: String = "",
    val position: Int,
    val visible: Boolean = true,
    /** all / video / audio / document / image / program / archive / other；自定义为 null。 */
    val builtinType: String? = null,
) {
    val isAll: Boolean get() = builtinType == "all"
    val isOther: Boolean get() = builtinType == "other"

    companion object {
        /** 与 `native/protocol/src/agent.rs::CustomCategoryDto::builtin_defaults` 同序同扩展名（主机未下发时的展示基线）。 */
        val BUILTIN: List<Category> = listOf(
            builtin("all", "folders", emptyList(), 0),
            builtin("video", "film", listOf("mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m3u8"), 1),
            builtin("audio", "music", listOf("mp3", "flac", "wav", "aac", "ogg", "m4a", "wma", "opus"), 2),
            builtin("document", "fileText", listOf("pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub", "md"), 3),
            builtin("image", "image", listOf("jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "heic", "avif"), 4),
            builtin("program", "cpu", listOf("exe", "msi", "dmg", "pkg", "deb", "rpm", "apk", "appimage"), 5),
            builtin("archive", "archive", listOf("zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso"), 6),
            builtin("other", "file", emptyList(), 7),
        )

        private fun builtin(type: String, icon: String, exts: List<String>, position: Int) =
            Category("builtin_$type", "", icon, exts, position = position, builtinType = type)
    }
}

/**
 * 网址通配符（`*` 任意串、`?` 单字符，其余字面；不分大小写；须完整匹配整个网址）：
 * 与 agent `category_dir.rs` / 引擎 `wildcard_matches` 同语义。空白 / 非法模式编译为 null（不命中）。
 */
object UrlWildcard {
    fun compile(pattern: String): Regex? {
        val trimmed = pattern.trim()
        if (trimmed.isEmpty()) return null
        val source = buildString(trimmed.length + 8) {
            for (ch in trimmed) {
                when (ch) {
                    '*' -> append(".*")
                    '?' -> append('.')
                    else -> append(Regex.escape(ch.toString()))
                }
            }
        }
        return runCatching { Regex(source, RegexOption.IGNORE_CASE) }.getOrNull()
    }
}

/**
 * 按 position 排序的匹配索引（语义同 GPUI `CategoryIndex`）：网址规则固定优先（规则间按列表顺序），
 * 其后是扩展名 / 正则规则（列表顺序）；都不命中 → other。网址规则匹配「来源地址」（调用方传
 * `Task.shareUrl`：`originUrl` 非空取它，否则取 `url`）；文件名未知（空串）时只有网址规则参与。
 * 规则（含正则 / 通配符编译）在构造时一次编译。
 */
class CategoryIndex(categories: List<Category>) {
    val ordered: List<Category> = categories.sortedBy { it.position }
    private val other: Category? = ordered.firstOrNull { it.isOther }

    private class Rule(val category: Category, val isUrl: Boolean, val test: (name: String, url: String) -> Boolean)

    private val urlRules: List<Rule>
    private val nameRules: List<Rule>
    private val ruleById: Map<String, Rule>

    init {
        val all = ordered
            .filter { !it.isAll && !it.isOther }
            .map { c -> Rule(c, c.urlPattern.isNotEmpty(), compile(c)) }
        urlRules = all.filter { it.isUrl }
        nameRules = all.filter { !it.isUrl }
        ruleById = HashMap<String, Rule>(all.size * 2).also { map -> all.forEach { map.putIfAbsent(it.category.id, it) } }
    }

    private fun compile(c: Category): (String, String) -> Boolean {
        if (c.urlPattern.isNotEmpty()) {
            val re = UrlWildcard.compile(c.urlPattern)
            return { _, url -> re != null && re.matches(url) }
        }
        if (c.regexPattern.isNotEmpty()) {
            val re = runCatching { Regex(c.regexPattern, RegexOption.IGNORE_CASE) }.getOrNull()
            return { name, _ -> name.isNotEmpty() && re?.containsMatchIn(name) == true }
        }
        val exts = c.extensions.map { it.lowercase().removePrefix(".") }.toSet()
        return { name, _ -> name.substringAfterLast('.', "").lowercase() in exts }
    }

    /** 任务命中的首个具体分类：网址规则优先，其后扩展名 / 正则；都不命中 → other（无 other 时 null）。 */
    fun categoryOf(fileName: String, url: String = ""): Category? =
        urlRules.firstOrNull { it.test(fileName, url) }?.category
            ?: categoryOfFileName(fileName)

    /** 仅按文件名匹配扩展名 / 正则，用于没有独立来源地址的文件（例如种子成员）。 */
    fun categoryOfFileName(fileName: String): Category? =
        nameRules.firstOrNull { it.test(fileName, "") }?.category
            ?: other

    /** 任务是否属于分类（all 恒真；other = 未命中任何具体分类）。 */
    fun matches(category: Category, fileName: String, url: String = ""): Boolean = when {
        category.isAll -> true
        category.isOther -> urlRules.none { it.test(fileName, url) } && nameRules.none { it.test(fileName, url) }
        else -> ruleById[category.id]?.test?.invoke(fileName, url) == true
    }
}
