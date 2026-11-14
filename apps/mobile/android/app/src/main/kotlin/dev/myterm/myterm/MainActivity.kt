package dev.myterm.myterm

import android.view.WindowManager
import androidx.biometric.BiometricPrompt
import io.flutter.embedding.android.FlutterFragmentActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * Native side of `lib/src/native.dart`. FlutterFragmentActivity is required
 * for BiometricPrompt. The method channel runs on the main thread, which is
 * where BiometricPrompt must be shown.
 */
class MainActivity : FlutterFragmentActivity() {

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "dev.myterm/native").setMethodCallHandler { call, result ->
            try {
                when (call.method) {
                    "hwGenerate" -> result.success(HardwareKeys.generate(call.argument<String>("alias")!!).toMap())

                    "hwList" -> result.success(HardwareKeys.list().map { it.toMap() })

                    "hwDelete" -> {
                        HardwareKeys.delete(call.argument<String>("alias")!!)
                        result.success(null)
                    }

                    "hwSign" -> {
                        val alias = call.argument<String>("alias")!!
                        val data = call.argument<ByteArray>("data")!!
                        val signature = HardwareKeys.signature(alias)
                        Biometric.authenticate(
                            this, "Sign in to server", "Hardware key $alias",
                            BiometricPrompt.CryptoObject(signature),
                        ) { ok, crypto ->
                            val s = crypto?.signature
                            if (!ok || s == null) {
                                result.success(ByteArray(0))
                            } else {
                                s.update(data)
                                result.success(s.sign()) // DER-encoded ECDSA signature
                            }
                        }
                    }

                    "deviceWrap" -> {
                        val mk = call.argument<ByteArray>("mk")!!
                        Biometric.authenticate(
                            this, "Enable biometric unlock", "Protect your MyTerm vault",
                            BiometricPrompt.CryptoObject(DeviceKey.encryptCipher()),
                        ) { ok, crypto ->
                            val c = crypto?.cipher
                            if (!ok || c == null) result.success(null) else result.success(c.iv + c.doFinal(mk))
                            mk.fill(0)
                        }
                    }

                    "deviceUnwrap" -> {
                        val blob = call.argument<ByteArray>("wrapped")!!
                        Biometric.authenticate(
                            this, "Unlock MyTerm", null,
                            BiometricPrompt.CryptoObject(DeviceKey.decryptCipher(blob)),
                        ) { ok, crypto ->
                            val c = crypto?.cipher
                            if (!ok || c == null) result.success(null)
                            else result.success(c.doFinal(blob, DeviceKey.IV_LEN, blob.size - DeviceKey.IV_LEN))
                        }
                    }

                    "confirmUser" -> {
                        val title = call.argument<String>("title") ?: "Confirm"
                        Biometric.authenticate(this, title, null, null) { ok, _ -> result.success(ok) }
                    }

                    "setSecure" -> {
                        if (call.argument<Boolean>("secure") == true) {
                            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
                        } else {
                            window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
                        }
                        result.success(null)
                    }

                    "keepAlive" -> {
                        SessionService.update(this, call.argument<Int>("sessions") ?: 0)
                        result.success(null)
                    }

                    else -> result.notImplemented()
                }
            } catch (e: Exception) {
                result.error("native", e.javaClass.simpleName + ": " + (e.message ?: ""), null)
            }
        }
    }
}
