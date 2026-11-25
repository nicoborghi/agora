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
