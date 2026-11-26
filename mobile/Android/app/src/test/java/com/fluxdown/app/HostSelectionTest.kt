package com.fluxdown.app

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.preferencesOf
import androidx.datastore.preferences.core.stringPreferencesKey
import com.fluxdown.app.data.HostRepo
import com.fluxdown.app.data.SecretBox
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.store.RemoteHostCodec
import com.fluxdown.core.store.RemoteHostRecord
import java.lang.reflect.Proxy
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HostSelectionTest {
    private val local = HostRef.Local("Android")
    private val saved = HostRef.Remote("saved", "Saved", "http://saved.example")
    private val other = HostRef.Remote("other", "Other", "http://other.example")

    @Test fun missingRemoteCannotReplaceSelectionAndRemovingSelectedHostClearsIt() = runBlocking {
        val repo = repo()
        repo.remove(other.id)
        repo.rememberRemote(other.id)
        assertEquals(saved, repo.lastRemote())
        repo.remove(saved.id)
        assertNull(repo.lastRemote())
    }

    @Test fun removingNewlyConfirmedRemoteAlsoClearsTheSelection() = runBlocking {
        val repo = repo()
        repo.rememberRemote(other.id)
        assertEquals(other, repo.lastRemote())
        repo.remove(other.id)
        assertNull(repo.lastRemote())
    }

    @Test fun failedRestoreLeavesLocalActiveAndRetainsChoiceForNextLaunch() = runBlocking {
        val fixture = Fixture(this) { Result.failure(IllegalStateException("offline")) }
        assertEquals(saved.displayName, fixture.selection.restoreLastHost())
        assertNull(fixture.selection.restoreLastHost())
        assertEquals(local, fixture.current)
        assertEquals(saved, fixture.repo.lastRemote())
    }

    @Test(timeout = 5_000) fun openingDownloadDraftKeepsItsHostWhenPendingRestoreFinishes() = runBlocking {
        val opening = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        var closed = false
        val fixture = Fixture(this) {
            opening.complete(Unit)
            release.await()
            Result.success(session { closed = true })
        }
        val restore = async { fixture.selection.restoreLastHost() }
        opening.await()
        val draftHost = fixture.selection.hostForNewDownloadDraft()
        release.complete(Unit)
        assertNull(restore.await())
        assertEquals(local, draftHost)
        assertEquals(draftHost, fixture.current)
        assertTrue(closed)
        assertEquals(saved, fixture.repo.lastRemote())
    }

    @Test(timeout = 5_000) fun keepingCurrentLocalHostCancelsPendingRestoreWithoutClearingChoice() = runBlocking {
        val opening = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val fixture = Fixture(this) {
            opening.complete(Unit)
            release.await()
            Result.success(session())
        }
        val restore = async { fixture.selection.restoreLastHost() }
        opening.await()
        assertTrue(fixture.selection.keepCurrentHost(local))
        release.complete(Unit)
        assertNull(restore.await())
        assertEquals(local, fixture.current)
        assertEquals(saved, fixture.repo.lastRemote())
    }

    @Test(timeout = 5_000) fun onlyHostSpecificNotificationDiscardsPendingRestore() = runBlocking {
        for (hostId in listOf("", local.id)) {
            val opening = CompletableDeferred<Unit>()
            val release = CompletableDeferred<Unit>()
            val fixture = Fixture(this) {
                opening.complete(Unit)
                release.await()
                Result.success(session())
            }
            val restore = async { fixture.selection.restoreLastHost() }
            opening.await()
            fixture.selection.prioritizeNotification(hostId)
            release.complete(Unit)
            assertNull(restore.await())
            assertEquals(if (hostId.isEmpty()) saved else local, fixture.current)
            assertEquals(saved, fixture.repo.lastRemote())
        }
    }

    @Test fun confirmingCurrentRemoteRemembersItWithoutReopeningTheSession() = runBlocking {
        var opened = 0
        val fixture = Fixture(this) {
            opened++
            Result.success(session())
        }
        assertTrue(fixture.selection.switchHost(other, remember = false).isSuccess)
        assertTrue(fixture.selection.keepCurrentHost(other))
        fixture.selection.rememberConfirmedRemote(other)
        assertEquals(other, fixture.current)
        assertEquals(other, fixture.repo.lastRemote())
        assertEquals(1, opened)
        assertNull(fixture.selection.restoreLastHost())
    }

    @Test fun notificationSwitchLeavesPreferenceAndManualReturnToLocalClearsIt() = runBlocking {
        val fixture = Fixture(this) { Result.success(session()) }
        assertTrue(fixture.selection.switchHost(other, remember = false).isSuccess)
        assertEquals(other, fixture.current)
        assertEquals(saved, fixture.repo.lastRemote())
        assertTrue(fixture.selection.switchHost(local).isSuccess)
        assertEquals(local, fixture.current)
        assertNull(fixture.repo.lastRemote())
        assertTrue(fixture.selection.switchHost(other).isSuccess)
        assertEquals(other, fixture.repo.lastRemote())
        assertNull(fixture.selection.restoreLastHost())
    }

    private inner class Fixture(scope: CoroutineScope, open: suspend (HostRef) -> Result<HostSession>) {
        val repo = repo()
        var current: HostRef = local
            private set
        val selection = HostSelection(scope, Mutex(), repo, { current }, { true }, open) { ref, _ -> current = ref }
    }

    private fun repo(): HostRepo {
        val records = listOf(saved, other).map { RemoteHostRecord(it, "synthetic") }
        val preferences = preferencesOf(
            stringPreferencesKey("hosts.remote") to RemoteHostCodec.encode(records),
            stringPreferencesKey("hosts.lastRemote") to saved.id,
        )
        return HostRepo(MemoryPreferencesStore(preferences), SecretBox())
    }

    private fun session(close: () -> Unit = {}): HostSession = Proxy.newProxyInstance(
        HostSession::class.java.classLoader,
        arrayOf(HostSession::class.java),
    ) { _, method, _ ->
        check(method.name == "close") { "Unexpected session call: ${method.name}" }
        close()
        null
    } as HostSession

    private class MemoryPreferencesStore(initial: Preferences) : DataStore<Preferences> {
        private val lock = Mutex()
        override val data = MutableStateFlow(initial)

        override suspend fun updateData(transform: suspend (Preferences) -> Preferences): Preferences =
            lock.withLock { transform(data.value).also { data.value = it } }
    }
}
