package org.tortotech.torto

import android.app.Application
import android.content.Context
import android.util.Base64
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.tortotech.torto.core.*

class TortoApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        NativePlatform.initializeTls(this)
        initializeCredentialStorage(File(filesDir,"core/config/credentials.json").absolutePath, LegacyKeystoreStorage(this))
        DiagnosticLog.install(this)
        DiagnosticLog.record(this,"app.start")
        CloudRuntime.initialize(this)
    }
}

/** Read-only importer. Rust writes all new credentials to the app-private JSON file. */
class LegacyKeystoreStorage(context: Context, preferencesName: String = "credentials", private val alias: String = "torto.credentials.v1") : LegacyCredentialStorage {
    private val preferences = context.getSharedPreferences(preferencesName, Context.MODE_PRIVATE)
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        return store.getKey(alias, null) as? SecretKey ?: error("Legacy credential key is missing")
    }
    private fun <T> guarded(block: () -> T): T = try { block() } catch (e: Exception) {
        throw CoreException.Operation("无法迁移旧凭据（${e.javaClass.simpleName}）")
    }
    @Synchronized override fun readAll(): Map<String,String> = guarded {
        val entries = preferences.all
        if (entries.isEmpty()) return@guarded emptyMap()
        val key = key()
        entries.mapValues { (name, value) ->
            val data = Base64.decode(value as? String ?: error("Invalid legacy credential"), Base64.NO_WRAP)
            require(data.size >= 28)
            Cipher.getInstance("AES/GCM/NoPadding").run {
                init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, data.copyOfRange(0,12)))
                updateAAD(name.toByteArray(Charsets.UTF_8))
                String(doFinal(data.copyOfRange(12,data.size)), Charsets.UTF_8)
            }
        }
    }
    @Synchronized override fun clear() = guarded {
        check(preferences.edit().clear().commit())
    }
}

/** JVM resources are supplied once; HTTPS continues to use Android's system trust store. */
internal object NativePlatform {
    init {System.loadLibrary("torto_android")}
    @JvmStatic external fun initializeTls(context:Context)
}
