package com.fluxdown.app.actions

import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PiecePickingMenuTest {
    private fun task(
        url: String = "magnet:?xt=urn:btih:abc",
        status: TaskStatus = TaskStatus.Downloading,
        sequential: Boolean = false,
        firstLast: Boolean = false,
        id: String = "t",
    ) = Task(
        taskId = id, url = url, originUrl = "", fileName = "a", saveDir = "/d",
        status = status, downloadedBytes = 1, totalBytes = 10,
        btSequential = sequential, btFirstLastPiecesFirst = firstLast,
    )

    @Test
    fun hiddenUnlessEverySelectedTaskIsIncompleteBt() {
        val bt = task()
        assertNull(piecePickingMenu(emptyList()))
        assertNull(piecePickingMenu(listOf(task(url = "https://example.com/a.bin"))))
        assertNull(piecePickingMenu(listOf(task(status = TaskStatus.Completed))))
        assertNull(piecePickingMenu(listOf(bt, task(url = "https://example.com/a.bin", id = "http"))))
        assertNull(piecePickingMenu(listOf(bt, task(status = TaskStatus.Completed, id = "done"))))
        assertEquals(
            PiecePickingMenu(order = PiecePickingOrder.Rarest, firstLastOn = false),
            piecePickingMenu(listOf(task(url = "torrent-file://abc"), task(status = TaskStatus.Paused, id = "p"), task(status = TaskStatus.Failed, id = "f"))),
        )
        assertEquals(
            PiecePickingMenu(order = PiecePickingOrder.Rarest, firstLastOn = false),
            piecePickingMenu(listOf(task(url = "MAGNET:?xt=urn:btih:ABC"))),
        )
    }

    @Test
    fun defaultAllOffSelectsRarest() {
        val menu = piecePickingMenu(listOf(task(), task(id = "b", firstLast = true)))
        assertEquals(PiecePickingOrder.Rarest, menu!!.order)
        assertFalse(menu.firstLastOn)
    }

    @Test
    fun allSequentialSelectsSequential() {
        val menu = piecePickingMenu(
            listOf(task(sequential = true, firstLast = true), task(id = "b", sequential = true)),
        )
        assertEquals(PiecePickingOrder.Sequential, menu!!.order)
        assertFalse(menu.firstLastOn)
    }

    @Test
    fun mixedOrderSelectsNeither() {
        val menu = piecePickingMenu(
            listOf(task(sequential = true), task(id = "b", sequential = false), task(id = "c", sequential = true)),
        )
        assertEquals(PiecePickingOrder.Mixed, menu!!.order)
    }

    @Test
    fun orderChangeSkipsMatchingTasksAndSendsOnlySequential() {
        val rarest = task(id = "rarest", firstLast = true)
        val sequential = task(id = "seq", sequential = true)
        val tasks = listOf(rarest, sequential)

        val toSequential = piecePickingOrderChange(tasks, sequential = true)
        assertEquals(listOf(rarest), toSequential!!.tasks)
        assertTrue(toSequential.sequential)
        assertEquals(mapOf("sequential" to true), toSequential.fields)
        assertFalse(toSequential.fields.containsKey("firstLastPiecesFirst"))

        val toRarest = piecePickingOrderChange(tasks, sequential = false)
        assertEquals(listOf(sequential), toRarest!!.tasks)
        assertFalse(toRarest.sequential)
        assertEquals(setOf("sequential"), toRarest.fields.keys)

        assertNull(piecePickingOrderChange(listOf(rarest), sequential = false))
        assertNull(piecePickingOrderChange(listOf(sequential, task(id = "seq2", sequential = true)), sequential = true))
        assertNull(piecePickingOrderChange(emptyList(), sequential = true))
    }

    @Test
    fun firstLastCheckIsAllOnAndToggleTargetIsTheInverse() {
        val mixed = piecePickingMenu(
            listOf(task(sequential = true, firstLast = true), task(id = "b", sequential = false, firstLast = true)),
        )
        assertEquals(PiecePickingOrder.Mixed, mixed!!.order)
        assertTrue(mixed.firstLastOn)
        assertFalse(mixed.firstLastTarget)

        val allOn = piecePickingMenu(listOf(task(sequential = true, firstLast = true)))
        assertEquals(PiecePickingOrder.Sequential, allOn!!.order)
        assertTrue(allOn.firstLastOn)
        assertFalse(allOn.firstLastTarget)

        val allOff = piecePickingMenu(listOf(task(firstLast = false), task(id = "b", firstLast = false)))
        assertEquals(PiecePickingOrder.Rarest, allOff!!.order)
        assertFalse(allOff.firstLastOn)
        assertTrue(allOff.firstLastTarget)
    }
}
