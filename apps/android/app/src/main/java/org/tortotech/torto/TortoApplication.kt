package org.tortotech.torto

import android.app.Application
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.tortotech.torto.core.*

class TortoApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        NativePlatform.initializeTls(this)
        initializeSecureStorage(KeystoreStorage(this))
        DiagnosticLog.install(this)
        DiagnosticLog.record(this,"app.start")
        CloudRuntime.initialize(this)
    }
}

/** Only ciphertext is written to private preferences. Keystore keys cannot be exported. */
class KeystoreStorage(context: Context) : SecureStorage {
    private val preferences = context.getSharedPreferences("credentials", Context.MODE_PRIVATE)
    private val alias = "torto.credentials.v1"
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
            generateKey()
        }
    }
    private fun id(service: String, account: String): String = MessageDigest.getInstance("SHA-256")
        .digest((service.length.toString() + ":" + service + account).toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }
    private fun <T> guarded(block: () -> T): T = try { block() } catch (e: Exception) {
        // Never include keys, passwords or encrypted payloads in errors/logs.
        throw CoreException.Operation("无法访问安全存储（${e.javaClass.simpleName}）")
    }
    @Synchronized override fun read(service: String, account: String): String? = guarded {
        val name = id(service, account)
        val stored = preferences.getString(name, null) ?: return@guarded null
        val data = Base64.decode(stored, Base64.NO_WRAP)
        require(data.size >= 28)
        Cipher.getInstance("AES/GCM/NoPadding").run {
            init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, data.copyOfRange(0, 12)))
            updateAAD(name.toByteArray(Charsets.UTF_8))
            String(doFinal(data.copyOfRange(12, data.size)), Charsets.UTF_8)
        }
    }
    @Synchronized override fun write(service: String, account: String, value: String) = guarded {
        val name = id(service, account)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply {
            init(Cipher.ENCRYPT_MODE, key()); updateAAD(name.toByteArray(Charsets.UTF_8))
        }
        val data = cipher.iv + cipher.doFinal(value.toByteArray(Charsets.UTF_8))
        check(preferences.edit().putString(name, Base64.encodeToString(data, Base64.NO_WRAP)).commit())
    }
    @Synchronized override fun delete(service: String, account: String) = guarded {
        check(preferences.edit().remove(id(service, account)).commit())
    }
}

/** JVM resources are supplied once; HTTPS continues to use Android's system trust store. */
internal object NativePlatform {
    init {System.loadLibrary("torto_android")}
    @JvmStatic external fun initializeTls(context:Context)
}
