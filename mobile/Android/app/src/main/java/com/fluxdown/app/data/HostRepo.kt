package com.fluxdown.app.data

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.store.RemoteHostCodec
import com.fluxdown.core.store.RemoteHostRecord
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import java.util.UUID

/**
 * 已保存的远端主机（`fluxdown-agent --server` / 开启局域网访问的桌面主机）：`hosts.remote` = JSON 数组
 * `[{id, name, endpoint, key, linkFingerprint?}]`（编解码见 [RemoteHostCodec]），`key` 为 [SecretBox] 密文（以 id 为 AAD），
 * 明文凭据从不落盘。凭据 = 手输的访问密钥，或局域网配对得到的设备控制凭据（此时带 `linkFingerprint`）。
 * 本机主机不入库（恒存在，id = [HostRef.Local.ID]）。
 */
class HostRepo(private val ds: DataStore<Preferences>, private val box: SecretBox) {
    private object K {
        val remote = stringPreferencesKey("hosts.remote")
        val lastRemote = stringPreferencesKey("hosts.lastRemote")
    }

    val remotes: Flow<List<HostRef.Remote>> = ds.data.map { p -> RemoteHostCodec.decode(p[K.remote]).map { it.ref } }

    internal suspend fun lastRemote(): HostRef.Remote? {
        val prefs = ds.data.first()
        val id = prefs[K.lastRemote] ?: return null
        return RemoteHostCodec.decode(prefs[K.remote]).firstOrNull { it.ref.id == id }?.ref
    }

    internal suspend fun rememberRemote(id: String?) {
        ds.edit { prefs ->
            if (id == null) prefs.remove(K.lastRemote) else prefs[K.lastRemote] = id
        }
    }

    /** 解密凭据；条目不存在或密钥已不可读（设备迁移 / Keystore 失效）→ null。 */
    suspend fun accessKey(id: String): String? {
        val entry = RemoteHostCodec.decode(ds.data.first()[K.remote]).firstOrNull { it.ref.id == id } ?: return null
        return try {
            box.open(entry.sealedKey, aad = id)
        } catch (_: SecretBox.SecretUnavailableException) {
            null
        }
    }

    /**
     * 新增一台主机。[linkFingerprint] 非空且已有同指纹的条目 = 同一台主机重新配对：原地更新该条目的地址与凭据
     * （保留 id 与名称），不新增一条。
     */
    suspend fun add(name: String, endpoint: String, accessKey: String, linkFingerprint: String? = null): HostRef.Remote {
        var result: HostRef.Remote? = null
        ds.edit { p ->
            val records = RemoteHostCodec.decode(p[K.remote])
            val existing = linkFingerprint?.let { fp -> RemoteHostCodec.idForFingerprint(records, fp) }
            val id = existing ?: UUID.randomUUID().toString()
            val displayName = records.firstOrNull { it.ref.id == existing }?.ref?.displayName ?: name
            val ref = HostRef.Remote(id = id, displayName = displayName, endpoint = endpoint, linkFingerprint = linkFingerprint)
            val record = RemoteHostRecord(ref, box.seal(accessKey, aad = id))
            p[K.remote] = RemoteHostCodec.encode(if (existing == null) records + record else records.map { if (it.ref.id == id) record else it })
            result = ref
        }
        return checkNotNull(result)
    }

    /** 原地更换访问密钥（主机侧已改密钥）；条目不存在 → false。保留地址与 `linkFingerprint`。 */
    suspend fun updateAccessKey(id: String, accessKey: String): Boolean {
        var found = false
        ds.edit { p ->
            val records = RemoteHostCodec.decode(p[K.remote]).map { e ->
                if (e.ref.id != id) {
                    e
                } else {
                    found = true
                    RemoteHostRecord(e.ref, box.seal(accessKey, aad = id))
                }
            }
            if (found) p[K.remote] = RemoteHostCodec.encode(records)
        }
        return found
    }

    /**
     * 重新配对成功：用新的设备控制凭据 / 指纹 / 地址原地替换同一台主机（id 与名称不变）；条目不存在 → null。
     */
    suspend fun replaceLinkCredential(id: String, endpoint: String, credential: String, linkFingerprint: String): HostRef.Remote? {
        var updated: HostRef.Remote? = null
        ds.edit { p ->
            val records = RemoteHostCodec.decode(p[K.remote]).map { e ->
                if (e.ref.id != id) {
                    e
                } else {
                    val ref = e.ref.copy(endpoint = endpoint, linkFingerprint = linkFingerprint)
                    updated = ref
                    RemoteHostRecord(ref, box.seal(credential, aad = id))
                }
            }
            if (updated != null) p[K.remote] = RemoteHostCodec.encode(records)
        }
        return updated
    }

    suspend fun remove(id: String) {
        ds.edit { p ->
            val kept = RemoteHostCodec.decode(p[K.remote]).filter { it.ref.id != id }
            if (kept.isEmpty()) p.remove(K.remote) else p[K.remote] = RemoteHostCodec.encode(kept)
            if (p[K.lastRemote] == id) p.remove(K.lastRemote)
        }
    }
}
