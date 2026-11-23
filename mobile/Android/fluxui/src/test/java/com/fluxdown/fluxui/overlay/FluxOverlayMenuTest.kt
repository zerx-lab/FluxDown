package com.fluxdown.fluxui.overlay

import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshots.Snapshot
import androidx.compose.ui.geometry.Rect
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test

class FluxOverlayMenuTest {
    @Test fun confirmedActionAppearsInTheAlreadyOpenMenu() {
        val overlay = FluxOverlayState()
        val confirmed = mutableStateOf(false)
        val details = FluxMenuItem.Action("Task Details", {})
        val open = FluxMenuItem.Action("Open File", {})
        val items = derivedStateOf { if (confirmed.value) listOf(open, details) else listOf(details) }

        overlay.showMenu(Rect.Zero, items)
        val shown = requireNotNull(overlay.menuEntry)
        assertEquals(listOf(details), shown.items)

        Snapshot.withMutableSnapshot { confirmed.value = true }
        assertSame(shown, overlay.menuEntry)
        assertEquals(listOf(open, details), shown.items)
    }

    @Test fun lateResultDoesNotReopenOrReplaceADismissedMenu() {
        val overlay = FluxOverlayState()
        val details = FluxMenuItem.Action("Task Details", {})
        val open = FluxMenuItem.Action("Open File", {})
        val items = mutableStateOf<List<FluxMenuItem>>(listOf(details))
        overlay.showMenu(Rect.Zero, items)
        overlay.dismissMenu()

        Snapshot.withMutableSnapshot { items.value = listOf(open, details) }
        assertNull(overlay.menuEntry)

        val other = FluxMenuItem.Action("Copy Link", {})
        overlay.showMenu(Rect.Zero, listOf(other))
        Snapshot.withMutableSnapshot { items.value = listOf(details) }
        assertEquals(listOf(other), overlay.menuEntry?.items)
    }
}
