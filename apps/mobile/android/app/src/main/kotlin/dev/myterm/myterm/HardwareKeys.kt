package dev.myterm.myterm

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec

/**
 * Hardware-bound SSH keys (spec 3.4.1): ECDSA P-256 in StrongBox when the
 * phone has it, otherwise the TEE. Every signature needs strong biometrics
 * or the device credential, and the key is invalidated if biometrics change.
 * The private key can never be exported.
 */
object HardwareKeys {
    private const val PREFIX = "myterm-ssh-"
    private val ALIAS_RE = Regex("[A-Za-z0-9._-]{1,64}")

    data class Info(val alias: String, val publicSec1: ByteArray, val protection: String) {
        fun toMap(): Map<String, Any> = mapOf("alias" to alias, "publicSec1" to publicSec1, "protection" to protection)
    }

    private fun keyStore(): KeyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    private fun spec(alias: String, strongBox: Boolean): KeyGenParameterSpec =
        KeyGenParameterSpec.Builder(PREFIX + alias, KeyProperties.PURPOSE_SIGN)
            .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(true)
            // 0 = authenticate for every single use, via BiometricPrompt + CryptoObject.
            .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
            .setInvalidatedByBiometricEnrollment(true)
            .setIsStrongBoxBacked(strongBox)
            .build()

    fun generate(alias: String): Info {
        require(ALIAS_RE.matches(alias)) { "invalid alias" }
        require(keyStore().getCertificate(PREFIX + alias) == null) { "key already exists" }
        val kpg = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, "AndroidKeyStore")
        try {
            kpg.initialize(spec(alias, strongBox = true))
            kpg.generateKeyPair()
        } catch (e: StrongBoxUnavailableException) {
            kpg.initialize(spec(alias, strongBox = false))
            kpg.generateKeyPair()
        }
        return info(alias) ?: error("key not found after generation")
    }

    fun info(alias: String): Info? {
        val ks = keyStore()
        val name = PREFIX + alias
        val cert = ks.getCertificate(name) ?: return null
        val pub = cert.publicKey as ECPublicKey
        val priv = ks.getKey(name, null) as PrivateKey
        val keyInfo = KeyFactory.getInstance(priv.algorithm, "AndroidKeyStore").getKeySpec(priv, KeyInfo::class.java)
        val protection = if (Build.VERSION.SDK_INT >= 31) {
            when (keyInfo.securityLevel) {
                KeyProperties.SECURITY_LEVEL_STRONGBOX -> "StrongBox"
                KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT -> "TEE"
                else -> "Software"
            }
        } else {
            @Suppress("DEPRECATION")
            if (keyInfo.isInsideSecureHardware) "TEE" else "Software"
        }
        return Info(alias, sec1(pub), protection)
    }

    /** SEC1 uncompressed point: 0x04 || X (32 bytes) || Y (32 bytes). */
    private fun sec1(pub: ECPublicKey): ByteArray {
        fun fixed(v: BigInteger): ByteArray {
            val raw = v.toByteArray()
            return when {
                raw.size == 32 -> raw
                raw.size > 32 -> raw.copyOfRange(raw.size - 32, raw.size)
                else -> ByteArray(32 - raw.size) + raw
            }
        }
        return byteArrayOf(0x04) + fixed(pub.w.affineX) + fixed(pub.w.affineY)
    }

    fun list(): List<Info> =
        keyStore().aliases().toList().filter { it.startsWith(PREFIX) }.mapNotNull { info(it.removePrefix(PREFIX)) }

    fun delete(alias: String) = keyStore().deleteEntry(PREFIX + alias)

    /** A Signature ready to hand to BiometricPrompt; usable only after the user authenticates. */
    fun signature(alias: String): Signature {
        val key = keyStore().getKey(PREFIX + alias, null) as? PrivateKey ?: error("no such key")
        return Signature.getInstance("SHA256withECDSA").apply { initSign(key) }
    }
}
