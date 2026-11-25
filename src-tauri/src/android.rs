//! Android-only glue: calls into `AgoraNative` (Kotlin, in gen/android) over JNI.
//! Plain JNI rather than a Tauri plugin: it works from any thread and never waits on the
//! UI thread, so it is safe during start-up and inside async commands alike.

use jni::objects::{JObject, JString, JValue};
use jni::JNIEnv;

const CLASS: &str = "app.agora.mobile.AgoraNative";

/// Runs `f` with a JNI environment, the application context (not the activity: AgoraNative
/// tracks that itself), and the `AgoraNative` class.
fn with_native<T>(f: impl FnOnce(&mut JNIEnv, &JObject, &JObject) -> jni::errors::Result<T>) -> Result<T, String> {
    let ctx = ndk_context::android_context();
    // SAFETY: tao fills the Android context with the process's JavaVM and the activity
    // before the app starts; both stay valid for the life of the process.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let run = |env: &mut JNIEnv| -> jni::errors::Result<T> {
        // App classes are only visible through the app's class loader (not FindClass on a
        // thread started from Rust).
        let loader = env.call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
        let name = env.new_string(CLASS)?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?
            .l()?;
        f(env, &activity, &class)
    };
    let result = env.with_local_frame(16, |env| run(env));
    // A Java exception must be cleared before the thread makes any other JNI call; its message
    // is the error (e.g. "No app on this phone opens .xyz files").
    if env.exception_check().unwrap_or(false) {
        let thrown = env.exception_occurred().ok();
        let _ = env.exception_clear();
        let message = thrown.and_then(|t| {
            let m = env.call_method(&t, "getMessage", "()Ljava/lang/String;", &[]).ok()?.l().ok()?;
            if m.is_null() {
                return None;
            }
            env.get_string(&JString::from(m)).ok().map(String::from)
        });
        let _ = env.exception_clear();
        if let Some(m) = message {
            return Err(m);
        }
    }
    result.map_err(|e| e.to_string())
}

/// Opens an http(s) link in the browser, or a downloaded file with the app Android picks.
pub fn open(target: &str) -> Result<(), String> {
    let link = target.starts_with("https://") || target.starts_with("http://");
    with_native(|env, activity, class| {
        let t = env.new_string(target)?;
        let method = if link { "openUrl" } else { "openFile" };
        call_static(env, class, method, "(Landroid/content/Context;Ljava/lang/String;)V", &[JValue::Object(activity), JValue::Object(&t)])?;
        Ok(())
    })
}

fn call_static<'a>(
    env: &mut JNIEnv<'a>,
    class: &JObject,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> jni::errors::Result<jni::objects::JValueOwned<'a>> {
    // SAFETY: `class` is the java.lang.Class returned by loadClass.
    let class = unsafe { jni::objects::JClass::from_raw(class.as_raw()) };
    env.call_static_method(&class, name, sig, args)
}

/// Secrets in the Android Keystore (see `AgoraNative.secretSet`).
pub struct KeystoreSecrets;

impl agora_core::secrets::SecretBackend for KeystoreSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        with_native(|env, activity, class| {
            let k = env.new_string(key)?;
            let v = call_static(env, class, "secretGet", "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;", &[
                JValue::Object(activity),
                JValue::Object(&k),
            ])?
            .l()?;
            if v.is_null() {
                return Ok(None);
            }
            let s: String = env.get_string(&JString::from(v))?.into();
            Ok(Some(s))
        })
        .map_err(|e| format!("Cannot read from secure storage: {e}"))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        with_native(|env, activity, class| {
            let k = env.new_string(key)?;
            let v = env.new_string(value)?;
            call_static(env, class, "secretSet", "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V", &[
                JValue::Object(activity),
                JValue::Object(&k),
                JValue::Object(&v),
            ])?;
            Ok(())
        })
        .map_err(|e| format!("Cannot save to secure storage: {e}"))
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        with_native(|env, activity, class| {
            let k = env.new_string(key)?;
            call_static(env, class, "secretDelete", "(Landroid/content/Context;Ljava/lang/String;)V", &[
                JValue::Object(activity),
                JValue::Object(&k),
            ])?;
            Ok(())
        })
    }
}

/// Forgets every web session cookie (after a sign-in, which keeps what it needs encrypted).
pub fn clear_cookies() {
    if let Err(e) = with_native(|env, _, class| {
        call_static(env, class, "clearCookies", "()V", &[])?;
        Ok(())
    }) {
        tracing::warn!("could not clear sign-in cookies: {e}");
    }
}

/// Sign-in pages see a desktop Chrome of the WebView's own version (built in AgoraNative), so
/// Slack and others show their web sign-in instead of "get the app" or "browser not supported".
pub const DESKTOP_AGENT: &str = "desktop";

/// Sets the WebView's browser identity (`None`: back to the app's own); applied before it returns.
pub fn set_user_agent(agent: Option<&str>) {
    if let Err(e) = with_native(|env, _, class| {
        let arg = match agent {
            Some(a) => JObject::from(env.new_string(a)?),
            None => JObject::null(),
        };
        call_static(env, class, "setUserAgent", "(Ljava/lang/String;)V", &[JValue::Object(&arg)])?;
        Ok(())
    }) {
        tracing::warn!("could not change the browser identity: {e}");
    }
}

/// Every non-empty cookie the WebView holds for `url`, as (name, value). Tauri's own cookie
/// reader crashes the app on Android when a page has none yet, so this asks CookieManager.
pub fn cookies(url: &str) -> Vec<(String, String)> {
    let raw = with_native(|env, _, class| {
        let u = env.new_string(url)?;
        let v = call_static(env, class, "cookies", "(Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&u)])?.l()?;
        let s: String = env.get_string(&JString::from(v))?.into();
        Ok(s)
    })
    .unwrap_or_default();
    raw.split(';')
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (!value.is_empty()).then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

/// Colours the status and navigation bars like Agora's own bars (`css` as "#rrggbb" or
/// "rgb(r, g, b)"), with light icons on a dark theme and dark icons on a light one.
pub fn set_bars(css: &str, dark: bool) {
    let Some(argb) = css_color(css) else { return };
    if let Err(e) = with_native(|env, _, class| {
        call_static(env, class, "setBars", "(IZ)V", &[JValue::Int(argb), JValue::Bool(u8::from(!dark))])?;
        Ok(())
    }) {
        tracing::warn!("could not colour the system bars: {e}");
    }
}

/// "#rrggbb", "#rgb" or "rgb(r, g, b)" as an opaque ARGB colour.
fn css_color(css: &str) -> Option<i32> {
    let css = css.trim();
    let (r, g, b) = if let Some(hex) = css.strip_prefix('#') {
        let hex: String = if hex.len() == 3 { hex.chars().flat_map(|c| [c, c]).collect() } else { hex.chars().take(6).collect() };
        let v = u32::from_str_radix(&hex, 16).ok()?;
        ((v >> 16) & 255, (v >> 8) & 255, v & 255)
    } else {
        let inner = css.strip_prefix("rgb")?.trim_start_matches('a').trim_start_matches('(').trim_end_matches(')');
        let mut it = inner.split(',').map(|p| p.trim().parse::<f32>().ok().map(|f| f.round() as u32));
        (it.next()??, it.next()??, it.next()??)
    };
    Some((0xFF00_0000u32 | (r << 16) | (g << 8) | b) as i32)
}

/// Whether App lock is on (Settings, Privacy & storage).
pub fn app_lock_on() -> bool {
    with_native(|env, activity, class| call_static(env, class, "appLockOn", "(Landroid/content/Context;)Z", &[JValue::Object(activity)])?.z())
        .unwrap_or(false)
}

/// Turns App lock on or off. Waits for the unlock prompt: call it off the async threads.
pub fn set_app_lock(on: bool) -> Result<(), String> {
    with_native(|env, activity, class| {
        call_static(env, class, "setAppLock", "(Landroid/content/Context;Z)V", &[JValue::Object(activity), JValue::Bool(u8::from(on))])?;
        Ok(())
    })
}
