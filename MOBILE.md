# Agora for Android: working notes

Status: in progress, not released. These notes record how desktop works today (step 1 of the plan)
and what the Android build changes. Agora is an unofficial client: check your workspace's policies.

## How desktop works today

**Sign-in** (`src-tauri/src/login.rs`, `commands.rs`): a private sign-in window loads the service's own
web login (SSO, 2FA and passwords work as in a browser), and Agora polls its cookies until the session
appears. Nothing is copied from another device.
- Slack: the web client's `xoxc-` token plus the `d` cookie. One login already adds **every workspace**
  of that login (Slack's teams cookie lists them all, one account each). Workspaces under a different
  email need their own login.
- Mattermost: the session cookie, or a pasted Personal Access Token (`token` argument).
- Rocket.Chat: user id + token cookies, or a pasted Personal Access Token (`user_id` + `token`).
- Several servers/accounts per service: yes, each account has its own id and token.

**Token storage** (`crates/agora-core/src/secrets.rs`): the system secret store via `keyring`
(Windows Credential Manager, macOS Keychain, Linux Secret Service), one entry per account
(`account:<id>`), plus the archive key. Never written to disk in clear.

**Archive**: SQLite (`agora.db`), encrypted with SQLCipher by default; its key is in the secret store.

**Search**: local full-text search (SQLite FTS) on the archive, instant; server search
(`Engine::search_remote`) is appended on demand with a "Search on the servers" button.

**Updates**: `tauri-plugin-updater` reads `latest.json` from the latest GitHub release and installs
only minisign-signed builds (`release.yml`, on `v*` tags).

## What changes on Android

| Desktop | Android |
| --- | --- |
| Sign-in window + cookie polling | In-app WebView sign-in (step 3) |
| `keyring` secret store | Android Keystore, with fingerprint only when App lock is on (step 3) |
| SQLCipher archive | Plain SQLite in the app's private storage (SQLCipher's OpenSSL does not cross-build easily); revisit with a Keystore-held key |
| TLS: Windows/system native-tls | rustls with the Mozilla root list (private/university CAs may need the platform verifier later) |
| Tray, notification popups, autostart, single instance, title bar colours, zoom | Not built |
| Background connections, notifications | None: everything is fetched when the app opens |
| tauri-plugin-updater | GitHub Releases check, APK download (step 8) |

Decisions: Android 10 or later, English only, mobile versions and tags separate from desktop,
no QR token handoff (sign in on the phone itself).

## Building for Android (needs Android Studio)

1. Install Android Studio, then in its SDK Manager: Android SDK Platform 34+, Build-Tools,
   Command-line Tools, **NDK (Side by side)**.
2. Set `JAVA_HOME` (Android Studio's `jbr` folder), `ANDROID_HOME` (the SDK folder) and
   `NDK_HOME` (`%ANDROID_HOME%\ndk\<version>`).
3. `rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android`
4. Once: `npm run tauri android init`
5. `npm run tauri android dev` (phone over USB or emulator) or `npm run tauri android build -- --apk`

### On this PC

- NDK 30.0.16248370, SDK platform 37, build-tools 36. The generated project lives in
  `src-tauri/gen/android` (git-ignored; `npx tauri android init --ci` recreates it).
- Debug APK, 64-bit phones only (fastest):
  `npx tauri android build --debug --target aarch64 --apk`
  → `src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`
- Live on the phone: enable Developer options → USB debugging, plug in, accept the prompt, then
  `npx tauri android dev` (UI edits reload; Rust edits rebuild).
- Install a built APK: `adb install -r <apk>` (`adb` is in `%ANDROID_HOME%\platform-tools`),
  or copy the file to the phone and open it.
- `tauri android build/dev` links the library with a symbolic link, which Windows only allows with
  **Developer Mode** on (Settings → System → For developers). Without it: copy
  `target/aarch64-linux-android/debug/libagora_lib.so` into
  `src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a/`, then in `src-tauri/gen/android` run
  `gradlew assembleUniversalDebug -x rustBuildUniversalDebug`.

## Step 3: sign-in and secrets on Android (done)

- Secrets: `src-tauri/src/android.rs` implements `agora_core::secrets::SecretBackend` by calling
  `AgoraNative` (Kotlin, `gen/android/.../AgoraNative.kt`) over JNI. Values are AES-256-GCM
  encrypted with a key that never leaves the Android Keystore; the ciphertext sits in private
  preferences. Backups are off (`android:allowBackup="false"`). Biometric binding comes with
  App lock (step 7).
- Sign-in: Android gives an app one WebView, so `mobile.rs::login` opens the service's page in
  Agora's own window, runs the same capture as desktop (cookies + the Slack/Rocket.Chat
  scripts, re-run each second) and returns to Agora. Going back cancels. Web cookies are wiped
  afterwards. A small plugin blocks `slack:`/`intent:` links so the service's app can't take over.
- Log: `adb shell run-as app.agora.mobile cat agora.log` (rewritten at each launch).

### Checking the phone layout over USB

- Debug builds stay on screen, even over the lock screen, while Agora is open, so
  `adb exec-out screencap -p > shot.png` works (this phone refuses "stay awake" over adb).
- The WebView can be inspected live: `adb forward tcp:9222 localabstract:webview_devtools_remote_<pid>`,
  then DevTools protocol on `http://127.0.0.1:9222/json` (or chrome://inspect). Measure overflow with
  `getBoundingClientRect()` against `innerWidth`, and test scrolling with `adb shell input swipe`.
- Back button: `adb shell input keyevent KEYCODE_BACK`; MainActivity asks `window.agoraBack()`.

## Step 4: fetch on open, Home recap (done, being tested)

- Nothing runs in the background. MainActivity reports onStart/onStop to `window.agoraForeground`,
  which calls `set_foreground`. A minute after leaving the screen `Engine::set_foreground` closes
  every connection; on return every workspace reconnects, which runs the usual catch-up.
  The minute keeps short trips (picking a photo, a glance at another app) connected.
- A cold start opens on Home, Catch-up (every workspace together).

## Step 5: chat by touch (done, being tested)

- Long press on a message shows its tools under it (quick reactions, emoji, bookmark, to-do, thread,
  more, edit, delete); text selection is off in messages ("Copy text" is in the more menu).
- Enter makes a new line; a Send button sends (main box and thread). Menus and pickers open as a
  sheet along the bottom; the emoji search does not open the keyboard by itself. Back closes them.
- Tested on the phone with the Demo workspace (added, then removed), never in real workspaces.

## Step 6: media, offline queue, archive limit (done, being tested)

- Links open in the phone's browser, downloaded attachments in the app Android picks for their
  type (`AgoraNative.openUrl` / `openFile`, through the FileProvider: read access to that file only).
  Pictures and PDFs still open inside Agora.
- Offline queue (desktop too): a message sent without a connection is kept in the archive
  (`kv` key "outbox"), shown greyed at the end of its conversation with Cancel, and sent in order
  once its workspace is connected (checked every 5 s while connected, every 30 s after a failure).
  Tested end to end with the Demo workspace.
- Archive size limit (`archive_mb`, Privacy & storage): 300 MB on Android, none on desktop. Past it
  the oldest messages go (never bookmarked or to-do ones), then the file is compacted; checked
  30 s after start, daily, and at once when the limit is lowered. Older history loads from the
  server again when scrolling up.

## Step 7: settings QR, App lock, theme (done, being tested)

- Theme: the status and navigation bars take the colour of Agora's bars, with dark icons on the
  light theme (`set_titlebar` → `AgoraNative.setBars`), and follow system dark mode changes.
- App lock (Privacy & storage, phone only): Android's fingerprint / face / PIN prompt when Agora
  opens and after 30 s away; content covered until unlocked; FLAG_SECURE keeps it out of
  screenshots and recent apps. Secrets move to a Keystore key usable only 5 min after an unlock
  (re-encrypted on switching); if needed later, the prompt appears again. Changed screen lock or
  fingerprints destroy that key: accounts then ask to sign in again.
- Settings QR (Advanced): the desktop shows a code with theme, accent, font, link previews, the
  All button and edit history (never tokens); the phone reads it with the camera through the
  WebView's BarcodeDetector (no extra library) and applies only those keys, type-checked.
- Note: the JNI context Rust gets is the Application, not the activity. AgoraNative tracks
  MainActivity itself for anything that needs the screen (prompts, bar colours).

## Step 8: signed release and update check (done)

### Signed release

1. Once: create your signing key (it asks for a password; keep the key and password safe and backed
   up: without them no update can ever be installed over this app again):
   `& 'C:Program FilesAndroidAndroid Studiojbrinkeytool.exe' -genkeypair -v -keystore $env:USERPROFILEgora-release.jks -alias agora -keyalg RSA -keysize 4096 -validity 10000`
2. Once: create `src-tauri/gen/android/keystore.properties` (git-ignored) with
   `storeFile=C:/Users/<you>/agora-release.jks` and `keyAlias=agora`. Give the password either as
   `storePassword=...` in that file, or as `$env:AGORA_KEYSTORE_PASSWORD` in the shell before building.
3. Set the phone's version in `src-tauri/tauri.android.conf.json` (separate from desktop).
4. `powershell -File scriptsndroid-release.ps1` → `distgora-android-<version>.apk` (about 16 MB,
   64-bit ARM, code shrinking on; checked that the Kotlin methods Rust calls survive it).
5. Publish a GitHub release tagged `android-v<version>` with that APK attached.

The first release-signed install needs the development build uninstalled first (Android refuses
to replace an app signed with a different key); sign in again afterwards. From then on, updates
install over it.

### Update check

When Agora opens (and every 6 hours while open) the phone asks GitHub for the newest release tagged
`android-v*` in the update feed's repository (desktop releases are ignored). If it is newer, the bar
above your name and Settings, Advanced offer Download: the APK opens in the browser and Android
installs it, only if it carries the same signature.

## Size and memory (measured 2026-11-25, on the phone)

| | Debug build | Optimised (release) build |
|---|---|---|
| Native library | 227 MB | 13.9 MB |
| APK | 28 MB fresh (251 MB after many in-place debug updates) | ~15 MB expected with code shrinking |
| Memory in use (PSS) | 191 MB | 135 MB |

Most of the remaining memory is Android's own WebView (Chromium), which any app built on web
technology carries. The interface files are 3.4 MB (PDF viewer 1.7 MB and emoji font 1.1 MB, each
loaded only when used). Debug packaging updates the APK in place and never compacts it: delete
the APK before packaging to measure. Image cache defaults to 256 MB on Android (1 GB on desktop).

## To do

- Slack multi-workspace sign-in works (bar with Done), but opening a second workspace makes Slack
  ask for the email again. Look for a way to reuse the first sign-in (e.g. Slack's workspace list
  after an email code, or `token_from_cookie` on each workspace of that email).
