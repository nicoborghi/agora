package app.agora.mobile

import android.app.KeyguardManager
import android.content.Context
import android.os.Build
import android.os.Looper
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_STRONG
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_WEAK
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * App lock (Settings, Privacy & storage): Android's own fingerprint / face / PIN screen guards
 * Agora, and the secrets move to a Keystore key that Android only releases for a few minutes
 * after such an unlock (see AgoraNative.key).
 */
object AppLock {
    private const val PREFS = "agora_lock"
    /** Back within this long after leaving: no new unlock. */
    const val GRACE_MS = 30_000L

    fun isOn(ctx: Context): Boolean = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean("on", false)

    fun setOn(ctx: Context, on: Boolean) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean("on", on).commit()
    }

    /** Whether the phone has a PIN, pattern, password or biometrics to unlock with. */
    fun deviceSecure(ctx: Context): Boolean = (ctx.getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager).isDeviceSecure

    // Android 10 cannot combine strong biometrics with the PIN in one prompt.
    private fun authenticators() =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) BIOMETRIC_STRONG or DEVICE_CREDENTIAL else BIOMETRIC_WEAK or DEVICE_CREDENTIAL

    private val waiting = mutableListOf<(Boolean) -> Unit>()
    private var showing = false

    /** Shows the unlock prompt (one at a time: later callers wait for the same one). */
    fun request(activity: FragmentActivity, onDone: (Boolean) -> Unit) {
        activity.runOnUiThread {
            waiting += onDone
            if (showing) return@runOnUiThread
            showing = true
            val prompt = BiometricPrompt(activity, ContextCompat.getMainExecutor(activity), object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = finish(true)
                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) = finish(false)
            })
            val info = BiometricPrompt.PromptInfo.Builder()
                .setTitle("Unlock Agora")
                .setAllowedAuthenticators(authenticators())
                .build()
            try {
                prompt.authenticate(info)
            } catch (e: Exception) {
                finish(false)
            }
        }
    }

    private fun finish(ok: Boolean) {
        showing = false
        val done = waiting.toList()
        waiting.clear()
        done.forEach { it(ok) }
    }

    /** Off the main thread only: shows the prompt and waits for the answer (2 minutes at most). */
    fun requestBlocking(activity: FragmentActivity): Boolean {
        check(Looper.myLooper() != Looper.getMainLooper()) { "unlock prompt waited on the main thread" }
        var ok = false
        val done = CountDownLatch(1)
        request(activity) { ok = it; done.countDown() }
        done.await(2, TimeUnit.MINUTES)
        return ok
    }
}
