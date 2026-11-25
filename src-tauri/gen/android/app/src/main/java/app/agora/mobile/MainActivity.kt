package app.agora.mobile

import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.webkit.WebView
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  private companion object {
    // Where Tauri serves Agora's own page on Android.
    const val APP_URL = "http://tauri.localhost/"
  }

  private var web: WebView? = null

  override fun onWebViewCreate(webView: WebView) {
    AgoraNative.attach(webView)
    web = webView
    // Back steps through Agora's own screens (window.agoraBack in app.js); at the top level
    // Agora goes to the background like other apps instead of closing.
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        webView.evaluateJavascript("window.agoraBack ? window.agoraBack() : false") { handled ->
          when {
            handled == "true" -> {}
            // A sign-in page (not Agora's): back cancels it and returns to Agora.
            webView.url?.startsWith(APP_URL) == false -> webView.loadUrl(APP_URL)
            else -> moveTaskToBack(true)
          }
        }
      }
    })
  }

  private var lastStop = 0L
  private var cover: View? = null

  // Agora fetches only while on screen: app.js tells the engine, which closes the connections a
  // minute after leaving and reconnects (catching up) on return. With App lock, only once unlocked.
  override fun onStart() {
    super.onStart()
    val away = System.currentTimeMillis() - lastStop
    if (AppLock.isOn(this) && (lastStop == 0L || away > AppLock.GRACE_MS)) lock() else foreground(true)
  }

  override fun onStop() {
    lastStop = System.currentTimeMillis()
    foreground(false)
    super.onStop()
  }

  private fun foreground(on: Boolean) {
    web?.evaluateJavascript("window.agoraForeground && window.agoraForeground($on)", null)
  }

  /** App lock: Agora stays covered until fingerprint, face or PIN; tapping the cover asks again. */
  private fun lock() {
    if (cover == null) {
      val v = TextView(this).apply {
        text = "Agora is locked\n\nTap to unlock"
        gravity = Gravity.CENTER
        textSize = 18f
        setTextColor(0xFFE4E4E7.toInt())
        setBackgroundColor(0xFF18181B.toInt())
        setOnClickListener { lock() }
      }
      addContentView(v, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
      cover = v
    }
    AppLock.request(this) { ok ->
      if (!ok) return@request
      (cover?.parent as? ViewGroup)?.removeView(cover)
      cover = null
      foreground(true)
    }
  }

  /** With App lock, Agora's screen is kept out of screenshots and the recent-apps preview. */
  fun applyLockSecurity() {
    if (AppLock.isOn(this)) window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    else window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    AgoraNative.attachActivity(this)
    applyLockSecurity()
    // Debug builds only: stay on screen (and over the lock screen) while connected for testing,
    // so the layout can be checked over USB. Release builds are unaffected.
    if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0) {
      window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
      setShowWhenLocked(true)
      setTurnScreenOn(true)
    }
    // Keep the app clear of the status bar, the camera cutout and the navigation bar
    // (Android 15+ always draws apps edge to edge, so this can't rely on the old behaviour).
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
      val bars = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime()
      )
      v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }
}
