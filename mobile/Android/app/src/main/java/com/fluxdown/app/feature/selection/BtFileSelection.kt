package com.fluxdown.app.feature.selection

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.triStateToggleable
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableStateSetOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.hostState
import com.fluxdown.app.ui.label
import com.fluxdown.app.ui.tileIcon
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.BtFile
import com.fluxdown.core.model.Category
import com.fluxdown.core.model.SelectionKind
import com.fluxdown.core.model.SelectionOutcome
import com.fluxdown.core.model.btNameOutcome
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxCheckbox
import com.fluxdown.fluxui.controls.FluxDivider
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlin.math.min

// ── 文件树模型 ─────────────────────────────────────────────────────────────

internal sealed interface BtNode {
    val name: String
    val depth: Int
}

internal class BtFolderNode(
    override val name: String,
    val path: String,
    override val depth: Int,
) : BtNode {
    val children = ArrayList<BtNode>()
    val indices = ArrayList<Int>()
    var size = 0L
    val folders = LinkedHashMap<String, BtFolderNode>()
}

internal class BtFileNode(override val name: String, override val depth: Int, val file: BtFile) : BtNode

/** 路径 `a/b/c.txt` 建树；目录在前（按名称），文件在后（保持清单顺序）；每个目录累计子树文件下标与大小。 */
internal fun buildBtTree(files: List<BtFile>): BtFolderNode {
    val root = BtFolderNode("", "", -1)
    for (f in files) {
        val parts = f.path.split('/').filter { it.isNotEmpty() }.ifEmpty { listOf(f.path) }
        var cur = root
        cur.indices += f.index
        cur.size += f.size
        for (seg in parts.dropLast(1)) {
            val child = cur.folders.getOrPut(seg) {
                BtFolderNode(seg, if (cur.path.isEmpty()) seg else "${cur.path}/$seg", cur.depth + 1).also { cur.children += it }
            }
            child.indices += f.index
            child.size += f.size
            cur = child
        }
        cur.children += BtFileNode(parts.last(), cur.depth + 1, f)
    }
    fun sortRec(folder: BtFolderNode) {
        val dirs = folder.children.filterIsInstance<BtFolderNode>().sortedBy { it.name.lowercase() }
        val fileNodes = folder.children.filterIsInstance<BtFileNode>()
        folder.children.clear()
        folder.children.addAll(dirs)
        folder.children.addAll(fileNodes)
        dirs.forEach(::sortRec)
    }
    sortRec(root)
    return root
}

private fun flatten(folder: BtFolderNode, collapsed: Set<String>, out: MutableList<BtNode>) {
    for (child in folder.children) {
        out += child
        if (child is BtFolderNode && child.path !in collapsed) flatten(child, collapsed, out)
    }
}

// ── X1 ───────────────────────────────────────────────────────────────────

private val btSizeChoices = listOf(0L to "", 10L shl 20 to "10 MB", 100L shl 20 to "100 MB", 1L shl 30 to "1 GB")

private val btOtherCategory = Category.BUILTIN.last { it.isOther }

private fun btCategory(path: String, rules: List<Category>, other: Category): Category {
    val name = path.substringAfterLast('/')
    val dot = name.lastIndexOf('.')
    if (dot <= 0 || dot == name.lastIndex) return other
    val extension = name.substring(dot + 1)
    return rules.firstOrNull { rule ->
        !rule.isAll && !rule.isOther && rule.regexPattern.isEmpty() && rule.urlPattern.isEmpty() &&
            rule.extensions.any { it.trimStart('.').equals(extension, ignoreCase = true) }
    } ?: other
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun BtFilters(
    query: String,
    onQuery: (String) -> Unit,
    rules: List<Category>,
    counts: Map<String, Int>,
    categories: Set<String>,
    toggleCategory: (String) -> Unit,
    minSize: Long,
    onSize: (Long) -> Unit,
) {
    FluxField(value = query, onValueChange = onQuery, label = str(R.string.webSelectionFilterPlaceholder))
    FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        for (category in rules) {
            val id = category.id
            val count = counts[id] ?: continue
            FluxButton(
                "${category.label()} $count", { toggleCategory(id) },
                variant = if (id in categories) ButtonVariant.Primary else ButtonVariant.Ghost, size = ButtonSize.Xs,
            )
        }
    }
    FluxText(str(R.string.btFilterMinSize), style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        for ((size, label) in btSizeChoices) {
            FluxButton(
                if (size == 0L) str(R.string.btFilterAnySize) else label, { onSize(size) },
                variant = if (size == minSize) ButtonVariant.Primary else ButtonVariant.Ghost, size = ButtonSize.Xs,
            )
        }
    }
}

/** X1：BT 文件选择（Full，可划走 = 取消；已改动选择后划走先确认）。 */
@Composable
internal fun BtSelectionSheet(ui: SelectionUi, kind: SelectionKind.Bt) {
    val request = ui.request
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val tree = remember(kind) { buildBtTree(kind.files) }
    val sizes = remember(kind) { kind.files.associate { it.index to it.size } }
    val allIndices = remember(kind) { kind.files.map { it.index } }
    // 只有清单里有目录时文件行才需要让出折叠箭头的位置；纯文件清单左对齐，不留空白。
    val chevronGutter = remember(tree) { if (tree.children.any { it is BtFolderNode }) CHEVRON_GUTTER else 0.dp }
    val initial = remember(request.requestId) {
        val d = (request.defaultChoice as? SelectionOutcome.Bt)?.indices.orEmpty().filter { it in sizes }
        if (d.isEmpty()) allIndices.toSet() else d.toSet()
    }
    val selected = remember(request.requestId) { mutableStateSetOf<Int>().also { it.addAll(initial) } }
    var expanded by remember(request.requestId) { mutableStateOf(false) }
    var query by remember(request.requestId) { mutableStateOf("") }
    var minSize by remember(request.requestId) { mutableStateOf(0L) }
    var filterCategories by remember(request.requestId) { mutableStateOf(emptySet<String>()) }
    val host = hostState()
    val categories by remember(host) { derivedStateOf { host.value.categories } }
    val rules = remember(categories) {
        val ordered = categories.ifEmpty { Category.BUILTIN }.sortedBy { it.position }
        if (ordered.any { it.isOther }) ordered else ordered + btOtherCategory
    }
    val other = rules.first { it.isOther }
    val categoryByIndex = remember(kind, rules) {
        kind.files.associate { it.index to btCategory(it.path, rules, other) }
    }
    val counts = remember(categoryByIndex) { categoryByIndex.values.groupingBy { it.id }.eachCount() }
    val needle = query.trim().lowercase()
    val filtering = expanded && (needle.isNotEmpty() || minSize > 0L || filterCategories.isNotEmpty())
    val visibleFiles = remember(kind, filtering, needle, minSize, filterCategories, categoryByIndex) {
        if (!filtering) kind.files else kind.files.filter {
            (minSize == 0L || it.size >= minSize) &&
                (filterCategories.isEmpty() || (categoryByIndex[it.index]?.id ?: other.id) in filterCategories) &&
                (needle.isEmpty() || it.path.lowercase().contains(needle))
        }
    }
    val visibleTree = remember(tree, visibleFiles, filtering) { if (filtering) buildBtTree(visibleFiles) else tree }
    val visibleIndices = remember(allIndices, visibleFiles, filtering) { if (filtering) visibleFiles.map { it.index } else allIndices }
    // 清单超过 60 项：默认只展开第 1 层（深度 ≥ 1 的目录折叠）
    val collapsed = remember(request.requestId) {
        mutableStateSetOf<String>().also { set ->
            if (kind.files.size > 60) {
                fun walk(f: BtFolderNode) {
                    for (ch in f.children) if (ch is BtFolderNode) {
                        if (ch.depth >= 1) set += ch.path
                        walk(ch)
                    }
                }
                walk(tree)
            }
        }
    }
    val rows by remember(visibleTree, filtering, collapsed) {
        derivedStateOf {
            ArrayList<BtNode>().also { flatten(visibleTree, if (filtering) emptySet() else collapsed, it) }
        }
    }
    val selectedBytes by remember(request.requestId) { derivedStateOf { selected.sumOf { sizes[it] ?: 0L } } }
    // 「名称」预填任务当前显示名；只有改过（去空白后非空且不同）才随确认下发 customName
    val task = ui.taskOf()
    val initialName = remember(request.requestId) { task?.fileName.orEmpty() }
    var name by remember(request.requestId) { mutableStateOf(initialName) }
    val dirty by remember(request.requestId) {
        derivedStateOf { selected.toSet() != initial || btNameOutcome(initialName, name) != null }
    }

    val count = selected.size
    val sizeText = Format.bytes(selectedBytes).toString()
    val screenH = LocalConfiguration.current.screenHeightDp
    val listHeight = (screenH - 380).coerceIn(220, 600).dp

    val close = { if (dirty) ui.resolver.confirmDiscard(request) else ui.resolver.cancel(request) }
    val title = str(R.string.btFileSelectTitle)
    val startedText = str(R.string.mobileDownloadStarted)

    FluxSheet(
        visible = ui.visible,
        onDismissRequest = close,
        detent = FluxSheetDetent.Full,
        title = title,
        header = {
            FluxSheetHeader(
                title = title,
                subtitle = if (kind.files.size == 1) str(R.string.btFileSelectDescSingle) else str(R.string.btFileSelectDesc, "count" to kind.files.size),
                onClose = close,
            )
        },
        footer = {
            SelectionFooter(ui.countdown) {
                FluxButton(str(R.string.cancel), { ui.resolver.cancelButton(request) }, modifier = Modifier.weight(1f))
                FluxButton(
                    str(R.string.btFileSelectConfirm, "count" to count, "size" to sizeText),
                    {
                        ui.resolver.resolve(
                            request,
                            SelectionOutcome.Bt(selected.toList().sorted(), saveDir = null, customName = btNameOutcome(initialName, name)),
                            startedText,
                            userInitiated = true,
                        )
                    },
                    modifier = Modifier.weight(2f),
                    variant = ButtonVariant.Primary,
                    enabled = count > 0,
                )
            }
        },
    ) {
        SelectionTaskRow(task)

        FluxField(
            value = name,
            onValueChange = { name = it },
            modifier = Modifier.padding(top = 8.dp),
            label = str(R.string.btFileColumnName),
        )

        if (kind.files.size > 1) {
            FluxButton(
                str(if (expanded) R.string.btFilterHide else R.string.btFilterShow),
                { expanded = !expanded }, variant = ButtonVariant.Ghost, size = ButtonSize.Xs,
            )
        }
        if (expanded) {
            BtFilters(query, { query = it }, rules, counts, filterCategories, { id ->
                filterCategories = if (id in filterCategories) filterCategories - id else filterCategories + id
            }, minSize, { minSize = it })
        }
        // 工具条
        Row(
            Modifier.fillMaxWidth().padding(top = 8.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            FluxButton(
                str(if (filtering) R.string.btFilterSelectVisible else R.string.btFileSelectAll),
                { selected.addAll(visibleIndices) }, variant = ButtonVariant.Ghost, size = ButtonSize.Xs,
                enabled = visibleIndices.isNotEmpty(),
            )
            FluxButton(
                str(if (filtering) R.string.btFilterClearVisible else R.string.deselectAll),
                { if (filtering) selected.removeAll(visibleIndices.toSet()) else selected.clear() },
                variant = ButtonVariant.Ghost, size = ButtonSize.Xs, enabled = visibleIndices.isNotEmpty(),
            )
            Spacer(Modifier.weight(1f))
            FluxText(
                str(R.string.selectedCount, "n" to count) + " · " + sizeText,
                style = type.sm,
                color = c.inkMuted,
                maxLines = 1,
            )
        }

        if (filtering && visibleFiles.isEmpty()) {
            FluxText(str(R.string.btFilterNoMatches), style = type.sm, color = c.inkMuted)
        }
        LazyColumn(
            Modifier
                .fillMaxWidth()
                .heightIn(max = listHeight)
                .clip(FluxTheme.shapes.card)
                .background(c.glass1),
        ) {
            items(rows, key = { n -> if (n is BtFolderNode) "d:${n.path}" else "f:${(n as BtFileNode).file.index}" }) { node ->
                when (node) {
                    is BtFolderNode -> FolderRow(
                        node = node,
                        state = folderState(node, selected),
                        collapsed = !filtering && node.path in collapsed,
                        onToggleSelect = {
                            if (folderState(node, selected) == ToggleableState.On) selected.removeAll(node.indices.toSet()) else selected.addAll(node.indices)
                        },
                        onToggleCollapse = if (filtering) null else {
                            { if (!collapsed.add(node.path)) collapsed.remove(node.path) }
                        },
                        isLast = node === rows.lastOrNull(),
                    )
                    is BtFileNode -> FileRow(
                        node = node,
                        checked = node.file.index in selected,
                        icon = categoryByIndex[node.file.index].tileIcon(bt = false),
                        gutter = chevronGutter,
                        isLast = node === rows.lastOrNull(),
                        onToggle = { if (!selected.add(node.file.index)) selected.remove(node.file.index) },
                    )
                }
            }
        }
    }
}

private fun folderState(node: BtFolderNode, selected: Set<Int>): ToggleableState {
    var n = 0
    for (i in node.indices) if (i in selected) n++
    return when (n) {
        0 -> ToggleableState.Off
        node.indices.size -> ToggleableState.On
        else -> ToggleableState.Indeterminate
    }
}

private fun indent(depth: Int) = (16 + min(depth, 4) * 16).dp

/** 目录行折叠箭头命中槽（48dp）+ 行内间距（10dp）：文件行勾选框与同层目录的勾选框对齐。 */
private val CHEVRON_GUTTER = 58.dp

@Composable
private fun FolderRow(
    node: BtFolderNode,
    state: ToggleableState,
    collapsed: Boolean,
    onToggleSelect: () -> Unit,
    onToggleCollapse: (() -> Unit)?,
    isLast: Boolean,
) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val haptics = FluxTheme.haptics
    val source = remember { MutableInteractionSource() }
    val expandLabel = if (collapsed) str(R.string.mobileExpand) else str(R.string.mobileCollapse)
    Column {
        Row(
            Modifier
                .fillMaxWidth()
                .height(50.dp)
                .triStateToggleable(state = state, interactionSource = source, indication = null, role = Role.Checkbox) {
                    haptics.tick()
                    onToggleSelect()
                }
                .padding(start = indent(node.depth), end = 14.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Box(
                Modifier
                    .size(width = 48.dp, height = 50.dp)
                    .then(if (onToggleCollapse == null) Modifier else Modifier.clickable(
                        role = Role.Button, onClickLabel = expandLabel, onClick = onToggleCollapse,
                    )),
                contentAlignment = Alignment.Center,
            ) {
                FluxIcon(
                    FluxIcons.ChevronDown,
                    null,
                    modifier = Modifier.graphicsLayer { rotationZ = if (collapsed) -90f else 0f },
                    size = 18.dp,
                    tint = c.inkMuted,
                )
            }
            FluxCheckbox(state, onClick = null, modifier = Modifier.clearAndSetSemantics { })
            FluxIcon(FluxIcons.FolderOpen, null, size = 18.dp, tint = c.inkMuted)
            FluxText(node.name, style = type.body, color = c.ink, maxLines = 1, modifier = Modifier.weight(1f))
            FluxText(Format.bytes(node.size).toString(), style = type.monoS, color = c.inkFaint, maxLines = 1)
        }
        if (!isLast) FluxDivider(startInset = indent(node.depth))
    }
}

@Composable
private fun FileRow(
    node: BtFileNode,
    checked: Boolean,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    gutter: Dp,
    isLast: Boolean,
    onToggle: () -> Unit,
) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val haptics = FluxTheme.haptics
    val source = remember { MutableInteractionSource() }
    Column {
        Row(
            Modifier
                .fillMaxWidth()
                .height(50.dp)
                .triStateToggleable(
                    state = if (checked) ToggleableState.On else ToggleableState.Off,
                    interactionSource = source,
                    indication = null,
                    role = Role.Checkbox,
                ) {
                    haptics.tick()
                    onToggle()
                }
                .padding(start = indent(node.depth) + gutter, end = 14.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            FluxCheckbox(checked, onCheckedChange = null, modifier = Modifier.clearAndSetSemantics { })
            FluxIcon(icon, null, size = 18.dp, tint = c.inkMuted)
            FluxText(node.name, style = type.body, color = c.ink, maxLines = 1, modifier = Modifier.weight(1f))
            FluxText(Format.bytes(node.file.size).toString(), style = type.monoS, color = c.inkFaint, maxLines = 1)
        }
        if (!isLast) FluxDivider(startInset = indent(node.depth) + gutter)
    }
}
