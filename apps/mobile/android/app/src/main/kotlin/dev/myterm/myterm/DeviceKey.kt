package dev.myterm.myterm

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Device key (spec 6.1): an AES-256-GCM key in StrongBox/TEE that wraps the
 * vault master key for everyday biometric unlock. Using it needs strong
 * biometrics or the device credential every time.
 * Wrapped format: IV (12 bytes) || ciphertext+tag.
 */
object DeviceKey {
    private const val ALIAS = "myterm-device-kek"
    const val IV_LEN = 12

    private fun spec(strongBox: Boolean) =
        KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setUserAuthenticationRequired(true)
            .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
            .setIsStrongBoxBacked(strongBox)
            .build()

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        return try {
            kg.init(spec(strongBox = true))
            kg.generateKey()
        } catch (e: StrongBoxUnavailableException) {
            kg.init(spec(strongBox = false))
            kg.generateKey()
        }
    }

    fun encryptCipher(): Cipher = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }

    fun decryptCipher(wrapped: ByteArray): Cipher {
        require(wrapped.size > IV_LEN + 16) { "wrapped key too short" }
        return Cipher.getInstance("AES/GCM/NoPadding").apply {
            init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, wrapped, 0, IV_LEN))
        }
    }
}
