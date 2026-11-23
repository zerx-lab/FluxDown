package com.fluxdown.app.feature.downloads

import com.fluxdown.core.protocol.RemoteTaskDto
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoteSelectionIdTest {
    @Test
    fun remoteItemIdsLiveInTheirOwnNamespace() {
        val item = RemoteItem(RemoteTaskDto("abc"), name = "a.zip", category = null, site = "", createdSec = 0)
        assertEquals("remote:abc", item.id)
        assertTrue(isRemoteSelectionId(item.id))
        // 本地任务 id（UUID / 数字）永不落入远程命名空间，本机守护进程调用不会收到远程 id。
        assertFalse(isRemoteSelectionId("0b8f4c1e-6d57-4f0a-9d0e-2f0b1c9d7a11"))
        assertFalse(isRemoteSelectionId("42"))
    }
}
