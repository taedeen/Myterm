package dev.myterm.myterm

import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_STRONG
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity

/** BiometricPrompt wrapper: strong biometrics, or the device PIN/pattern as fallback. */
object Biometric {
    fun authenticate(
        activity: FragmentActivity,
        title: String,
        subtitle: String?,
        crypto: BiometricPrompt.CryptoObject?,
        done: (Boolean, BiometricPrompt.CryptoObject?) -> Unit,
    ) {
        var finished = false
        val callback = object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                if (!finished) {
                    finished = true
                    done(true, result.cryptoObject)
                }
            }

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                if (!finished) {
                    finished = true
                    done(false, null)
                }
            }
            // onAuthenticationFailed = one bad attempt; the prompt stays open.
        }
        val prompt = BiometricPrompt(activity, ContextCompat.getMainExecutor(activity), callback)
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle(title)
            .apply { if (subtitle != null) setSubtitle(subtitle) }
            .setAllowedAuthenticators(BIOMETRIC_STRONG or DEVICE_CREDENTIAL)
            .build()
        if (crypto != null) prompt.authenticate(info, crypto) else prompt.authenticate(info)
    }
}
