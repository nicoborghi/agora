package app.agora.mobile

import android.content.ActivityNotFoundException
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import androidx.core.view.WindowCompat
import java.io.File
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.security.keystore.UserNotAuthenticatedException
import androidx.fragment.app.FragmentActivity
import android.util.Base64
import android.webkit.CookieManager
import android.webkit.WebView
import androidx.webkit.UserAgentMetadata
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewFeature
import java.lang.ref.WeakReference
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Called from Rust over JNI (src-tauri/src/android.rs).
 *
 * Secrets (session tokens, cookies) are encrypted with an AES-256-GCM key that lives in the
 * Android Keystore and never leaves it; only the ciphertext is kept, in private app
 * preferences that are excluded from backups.
 */
object AgoraNative {
    private const val ALIAS = "agora_secrets"
    private const val PREFS = "agora_secrets"
    private var webView: WeakReference<WebView>? = null
    // Rust only has the Application as its context: the screen is tracked here (MainActivity).
    private var activityRef: WeakReference<MainActivity>? = null

    fun attachActivity(activity: MainActivity) {
        activityRef = WeakReference(activity)
    }

    private fun screen(): MainActivity? = activityRef?.get()
    private var appAgent: String? = null

    fun attach(view: WebView) {
        webView = WeakReference(view)
    }

    /** Key for the secrets: without App lock always usable; with it, only for a few minutes
     *  after the phone or Agora was unlocked with fingerprint, face or PIN. */
    private const val LOCKED_ALIAS = "agora_secrets_locked"
    private const val UNLOCK_VALID_S = 300

    private fun alias(ctx: Context) = if (AppLock.isOn(ctx)) LOCKED_ALIAS else ALIAS

    private fun key(alias: String): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(alias, null) as? SecretKey)?.let { return it }
        val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
        if (alias == LOCKED_ALIAS) {
            spec.setUserAuthenticationRequired(true)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                spec.setUserAuthenticationParameters(UNLOCK_VALID_S, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
            } else {
                @Suppress("DEPRECATION")
                spec.setUserAuthenticationValidityDurationSeconds(UNLOCK_VALID_S)
            }
        }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(spec.build())
        return gen.generateKey()
    }

    private fun deleteKey(alias: String) {
        KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(alias)
    }

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /**
     * Runs a Keystore operation; when Android wants a fresh unlock first (App lock), shows the
     * unlock prompt and tries once more. On the main thread it cannot wait, so it reports locked.
     */
    private fun <T> unlocked(ctx: Context, op: () -> T): T = try {
        op()
    } catch (e: UserNotAuthenticatedException) {
        val activity = screen()
        if (activity == null || Looper.myLooper() == Looper.getMainLooper() || !AppLock.requestBlocking(activity)) {
            throw IllegalStateException("Agora is locked")
        }
        op()
    }

    private fun seal(alias: String, value: String): String {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key(alias))
        return Base64.encodeToString(cipher.iv + cipher.doFinal(value.toByteArray(Charsets.UTF_8)), Base64.NO_WRAP)
    }

    private fun open(alias: String, stored: String): String {
        val sealed = Base64.decode(stored, Base64.NO_WRAP)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(alias), GCMParameterSpec(128, sealed, 0, 12))
        return String(cipher.doFinal(sealed, 12, sealed.size - 12), Charsets.UTF_8)
    }

    @JvmStatic
    fun secretSet(ctx: Context, name: String, value: String) {
        val sealed = unlocked(ctx) { seal(alias(ctx), value) }
        if (!prefs(ctx).edit().putString(name, sealed).commit()) {
            throw IllegalStateException("could not save the secret")
        }
    }

    @JvmStatic
    fun secretGet(ctx: Context, name: String): String? {
        val stored = prefs(ctx).getString(name, null) ?: return null
        return try {
            unlocked(ctx) { open(alias(ctx), stored) }
        } catch (e: KeyPermanentlyInvalidatedException) {
            // The phone's screen lock or fingerprints changed: Android destroyed the key, so the
            // saved sessions cannot be read any more. Forget them: each account asks to sign in again.
            deleteKey(alias(ctx))
            prefs(ctx).edit().clear().commit()
            null
        }
    }

    @JvmStatic
    fun secretDelete(ctx: Context, name: String) {
        prefs(ctx).edit().remove(name).commit()
    }

    @JvmStatic
    fun appLockOn(ctx: Context): Boolean = AppLock.isOn(ctx)

    /**
     * Turns App lock on or off (off the main thread: it waits for the unlock prompt). Every saved
     * secret is decrypted with the current key and encrypted again with the other one.
     */
    @JvmStatic
    fun setAppLock(ctx: Context, on: Boolean) {
        if (AppLock.isOn(ctx) == on) return
        val activity = screen() ?: throw IllegalStateException("Agora is not on screen")
        if (on && !AppLock.deviceSecure(ctx)) throw IllegalStateException("Set a screen lock (PIN, pattern or fingerprint) on the phone first")
        if (!AppLock.requestBlocking(activity)) throw IllegalStateException("Not unlocked: App lock unchanged")
        val from = alias(ctx)
        val to = if (on) LOCKED_ALIAS else ALIAS
        val plain = prefs(ctx).all.mapNotNull { (k, v) -> (v as? String)?.let { k to unlocked(ctx) { open(from, it) } } }
        deleteKey(to)
        val edit = prefs(ctx).edit()
        for ((k, v) in plain) edit.putString(k, unlocked(ctx) { seal(to, v) })
        if (!edit.commit()) throw IllegalStateException("could not save the secrets")
        AppLock.setOn(ctx, on)
        deleteKey(from)
        activity.runOnUiThread { activity.applyLockSecurity() }
    }

    /** The cookies the WebView would send to `url` ("a=1; b=2"), or "" when there are none. */
    @JvmStatic
    fun cookies(url: String): String = CookieManager.getInstance().getCookie(url) ?: ""

    /** Forgets the web sessions left by a sign-in page (Agora keeps what it needs encrypted). */
    @JvmStatic
    fun clearCookies() {
        val cm = CookieManager.getInstance()
        cm.removeAllCookies(null)
        cm.flush()
    }

    private var appMetadata: UserAgentMetadata? = null

    /** The colour behind the status and navigation bars, and whether their icons are dark. */
    @JvmStatic
    fun setBars(color: Int, lightBars: Boolean) {
        val activity = screen() ?: return
        activity.runOnUiThread {
            val window = activity.window
            window.decorView.setBackgroundColor(color)
            activity.findViewById<android.view.View>(android.R.id.content)?.setBackgroundColor(color)
            WindowCompat.getInsetsController(window, window.decorView).apply {
                isAppearanceLightStatusBars = lightBars
                isAppearanceLightNavigationBars = lightBars
            }
        }
    }

    /** Opens an http(s) link in the phone's browser. */
    @JvmStatic
    fun openUrl(ctx: Context, url: String) {
        ctx.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    /**
     * Opens a downloaded attachment (in Agora's cache folder) with the app Android picks for its
     * type, through the FileProvider: that app gets read access to this one file only.
     */
    @JvmStatic
    fun openFile(ctx: Context, path: String) {
        val file = File(path)
        val uri = FileProvider.getUriForFile(ctx, ctx.packageName + ".fileprovider", file)
        val mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "*/*"
        val view = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mime)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            ctx.startActivity(view)
        } catch (e: ActivityNotFoundException) {
            throw IllegalStateException("No app on this phone opens .${file.extension} files")
        }
    }

    /**
     * Sign-in pages see a desktop Chrome of the WebView's own version ("desktop"), both in the
     * user-agent string and in the client hints, so services show their web sign-in instead of
     * "get the app" or "browser not supported". `null` restores the app's own identity. Waits
     * until it is applied, so the page loaded next already gets it.
     */
    @JvmStatic
    fun setUserAgent(agent: String?) {
        val view = webView?.get() ?: return
        val apply = Runnable {
            val s = view.settings
            val hints = WebViewFeature.isFeatureSupported(WebViewFeature.USER_AGENT_METADATA)
            if (appAgent == null) {
                appAgent = s.userAgentString
                if (hints) appMetadata = WebSettingsCompat.getUserAgentMetadata(s)
            }
            if (agent == null) {
                s.userAgentString = appAgent
                if (hints) appMetadata?.let { WebSettingsCompat.setUserAgentMetadata(s, it) }
                return@Runnable
            }
            val full = Regex("Chrome/([0-9.]+)").find(appAgent ?: "")?.groupValues?.get(1) ?: "140.0.0.0"
            val major = full.substringBefore('.')
            s.userAgentString = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/$major.0.0.0 Safari/537.36"
            if (hints) {
                fun brand(name: String, v: String) =
                    UserAgentMetadata.BrandVersion.Builder().setBrand(name).setMajorVersion(v.substringBefore('.')).setFullVersion(v).build()
                WebSettingsCompat.setUserAgentMetadata(
                    s,
                    UserAgentMetadata.Builder()
                        .setBrandVersionList(listOf(brand("Chromium", full), brand("Google Chrome", full), brand("Not.A/Brand", "99.0.0.0")))
                        .setFullVersion(full)
                        .setPlatform("Windows")
                        .setPlatformVersion("10.0.0")
                        .setArchitecture("x86")
                        .setBitness(64)
                        .setModel("")
                        .setMobile(false)
                        .build()
                )
            }
        }
        if (Looper.myLooper() == Looper.getMainLooper()) { apply.run(); return }
        val done = CountDownLatch(1)
        Handler(Looper.getMainLooper()).post { apply.run(); done.countDown() }
        done.await(2, TimeUnit.SECONDS)
    }
}
