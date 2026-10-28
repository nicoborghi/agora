//! Commands invoked from the UI. Thin wrappers over the engine; all async so
//! they run on the Tokio runtime and never block the window thread.

use crate::{login, popup, tray, AppState};
use agora_core::engine::{ChannelDetailsView, ChannelView, HomeItem, MembersView, MessageView, PopupNote, ProfileView, SelfView};
use agora_core::model::{Account, AccountId, Canvas, ConnState, Millis, Presence, ServiceKind, User};
use agora_core::settings::{ChannelPrefs, Settings};
use agora_core::{secrets, store::Store};
use serde::Serialize;
use std::collections::HashMap;
use tauri::{AppHandle, Emitter, Manager, State};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct SettingsDto {
    #[serde(flatten)]
    settings: Settings,
    db_encrypted: bool,
    cipher_available: bool,
    data_dir: String,
    version: String,
}

#[derive(Serialize)]
pub struct BootstrapDto {
    accounts: Vec<Account>,
    connections: HashMap<AccountId, ConnState>,
    channels: Vec<ChannelView>,
    settings: SettingsDto,
}

fn settings_dto(app: &AppHandle, state: &AppState) -> SettingsDto {
    SettingsDto {
        settings: state.engine.settings(),
        db_encrypted: state.boot.lock().map(|b| b.db_encrypted).unwrap_or(false),
        cipher_available: Store::cipher_available(),
        data_dir: state.data_dir.display().to_string(),
        version: app.package_info().version.to_string(),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn bootstrap(app: AppHandle, state: State<'_, AppState>) -> CmdResult<BootstrapDto> {
    Ok(BootstrapDto {
        accounts: state.engine.accounts(),
        connections: state.engine.connections(),
        channels: state.engine.channels(None)?,
        settings: settings_dto(&app, &state),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_accounts(state: State<'_, AppState>) -> CmdResult<Vec<Account>> {
    Ok(state.engine.accounts())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_channels(state: State<'_, AppState>, account_id: Option<String>) -> CmdResult<Vec<ChannelView>> {
    state.engine.channels(account_id.as_deref())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_messages(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    before: Option<Millis>,
    limit: Option<u32>,
) -> CmdResult<Vec<MessageView>> {
    state.engine.messages(&account_id, &channel_id, before, limit.unwrap_or(50).min(200)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_thread(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    root_id: String,
) -> CmdResult<Vec<MessageView>> {
    state.engine.thread(&account_id, &channel_id, &root_id).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn open_channel(
    state: State<'_, AppState>,
    account_id: Option<String>,
    channel_id: Option<String>,
) -> CmdResult<()> {
    state.engine.open_channel(account_id.zip(channel_id))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn send_message(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    text: String,
    thread_root: Option<String>,
) -> CmdResult<MessageView> {
    state.engine.send(&account_id, &channel_id, &text, thread_root.as_deref()).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn edit_message(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    message_id: String,
    text: String,
) -> CmdResult<()> {
    state.engine.edit(&account_id, &channel_id, &message_id, &text).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn delete_message(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    message_id: String,
) -> CmdResult<()> {
    state.engine.delete(&account_id, &channel_id, &message_id).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn react(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    message_id: String,
    emoji: String,
    add: bool,
) -> CmdResult<()> {
    state.engine.react(&account_id, &channel_id, &message_id, &emoji, add).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn preview_remote(state: State<'_, AppState>, account_id: String, text: String) -> CmdResult<String> {
    Ok(state.engine.preview_remote(&account_id, &text))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_starred(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    starred: bool,
) -> CmdResult<()> {
    state.engine.set_starred(&account_id, &channel_id, starred).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_local_group(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    group: Option<String>,
) -> CmdResult<()> {
    state.engine.set_local_group(&account_id, &channel_id, group.as_deref())
}

/// `remote` asks the services instead of the archive.
#[tauri::command(rename_all = "snake_case")]
pub async fn search_messages(
    state: State<'_, AppState>,
    query: String,
    account_id: Option<String>,
    remote: bool,
) -> CmdResult<Vec<MessageView>> {
    if remote {
        state.engine.search_remote(&query, account_id.as_deref(), 50).await
    } else {
        let engine = state.engine.clone();
        tauri::async_runtime::spawn_blocking(move || engine.search(&query, account_id.as_deref(), 50))
            .await
            .map_err(err)?
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current: String,
    pub notes: Option<String>,
}

/// Asks the update feed (Settings → Advanced, a GitHub release's latest.json) for a newer,
/// signed version. Remembers it so `install_update` can install it; `None` when up to date.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_update(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<UpdateInfo>> {
    use tauri_plugin_updater::UpdaterExt;
    let mut builder = app.updater_builder();
    if let Ok(url) = state.engine.settings().update_url.parse::<tauri::Url>() {
        builder = builder.endpoints(vec![url]).map_err(err)?;
    }
    let update = builder.build().map_err(err)?.check().await.map_err(err)?;
    let info = update.as_ref().map(|u| UpdateInfo { version: u.version.clone(), current: u.current_version.clone(), notes: u.body.clone() });
    *state.update.lock().unwrap_or_else(|e| e.into_inner()) = update;
    Ok(info)
}

/// Downloads the update found by `check_update`, checks its signature, runs the installer and
/// restarts Agora. Settings, archive and sign-ins are outside the program folder and stay.
#[tauri::command(rename_all = "snake_case")]
pub async fn install_update(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let update = state.update.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or("No update to install")?;
    update.download_and_install(|_, _| {}, || {}).await.map_err(err)?;
    app.restart()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_bookmarks(state: State<'_, AppState>) -> CmdResult<Vec<MessageView>> {
    state.engine.bookmarks()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_bookmark(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    message_id: String,
    on: bool,
) -> CmdResult<()> {
    state.engine.set_bookmark(&account_id, &channel_id, &message_id, on)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_me(state: State<'_, AppState>, account_id: String) -> CmdResult<Option<SelfView>> {
    state.engine.me(&account_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_profile(state: State<'_, AppState>, account_id: String, user_id: String) -> CmdResult<ProfileView> {
    state.engine.profile(&account_id, &user_id).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn channel_members(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    cursor: Option<String>,
) -> CmdResult<MembersView> {
    state.engine.members(&account_id, &channel_id, cursor.as_deref()).await
}

/// One canvas, from the Details panel or a canvas link in a message.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_canvas(state: State<'_, AppState>, account_id: String, id: String) -> CmdResult<Canvas> {
    state.engine.canvas(&account_id, &id).await
}

/// The Details panel of a conversation: description, creator, bookmarks, pins.
#[tauri::command(rename_all = "snake_case")]
pub async fn channel_details(state: State<'_, AppState>, account_id: String, channel_id: String) -> CmdResult<ChannelDetailsView> {
    state.engine.channel_details(&account_id, &channel_id).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_presence(state: State<'_, AppState>, account_id: String, presence: Presence) -> CmdResult<()> {
    state.engine.set_presence(&account_id, presence).await
}

/// Goes back to the service's own logo, fetched now when connected.
#[tauri::command(rename_all = "snake_case")]
pub async fn use_service_icon(state: State<'_, AppState>, account_id: String) -> CmdResult<()> {
    let mut account = state.engine.accounts().into_iter().find(|a| a.id == account_id).ok_or("unknown account")?;
    account.icon_image = None;
    state.engine.update_account(account)?;
    // Offline: the logo arrives with the next connection instead.
    let _ = state.engine.refresh_icon(&account_id).await;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_channel_prefs(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
) -> CmdResult<ChannelPrefs> {
    state.engine.channel_prefs(&account_id, &channel_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_channel_prefs(
    state: State<'_, AppState>,
    account_id: String,
    channel_id: String,
    prefs: ChannelPrefs,
) -> CmdResult<()> {
    state.engine.set_channel_prefs(&account_id, &channel_id, &prefs)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn save_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<()> {
    state.engine.save_settings(settings)
}

/// Encrypts or decrypts the archive in place, then remembers the choice.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_db_encryption(state: State<'_, AppState>, enabled: bool) -> CmdResult<bool> {
    let current = state.boot.lock().map_err(err)?.db_encrypted;
    if current == enabled {
        return Ok(enabled);
    }
    if enabled && !Store::cipher_available() {
        return Err("this build was compiled without SQLCipher".into());
    }
    let key = secrets::db_key()?;
    let old_key = current.then(|| key.clone());
    let new_key = enabled.then_some(key);
    let store = state.engine.store();
    tauri::async_runtime::spawn_blocking(move || store.convert(old_key.as_deref(), new_key.as_deref()))
        .await
        .map_err(err)?
        .map_err(err)?;
    let mut boot = state.boot.lock().map_err(err)?;
    boot.db_encrypted = enabled;
    boot.save(&state.data_dir).map_err(err)?;
    Ok(enabled)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn add_demo_account(state: State<'_, AppState>) -> CmdResult<Account> {
    let n = state.engine.accounts().len() as i64;
    let account = Account::new(ServiceKind::Demo, "Demo workspace", "demo://local", n);
    state.engine.add_account(account.clone())?;
    Ok(account)
}

#[derive(Serialize)]
pub struct SignInDto {
    account: Account,
    username: String,
}

fn host(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url).to_string()
}

/// Adds a Mattermost account, or refreshes the token of an existing one (`account_id`,
/// or the same user on the same server). Without `token`, opens the server's login page.
#[tauri::command(rename_all = "snake_case")]
pub async fn mattermost_sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    server: String,
    token: Option<String>,
    account_id: Option<String>,
) -> CmdResult<SignInDto> {
    let info = agora_mattermost::probe_server(&server).await?;
    let token = match token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => {
            let title = format!("Sign in to {}", info.site_name);
            let start = format!("{}/login", info.url);
            let sets: &[&[&str]] = &[&[agora_mattermost::SESSION_COOKIE]];
            login::capture_cookies(&app, &title, &start, sets, None).await?.remove(0)
        }
    };
    let who = agora_mattermost::whoami(&info.url, &token).await?;
    let name = match who.teams.as_slice() {
        [team] => team.clone(),
        _ if info.site_name != "Mattermost" => info.site_name.clone(),
        _ => host(&info.url),
    };
    let signed = SignedIn { kind: ServiceKind::Mattermost, server: info.url, user_id: who.user_id, username: who.username, name, team_id: None };
    save_sign_in(&state, signed, &token, account_id)
}

/// Adds a Rocket.Chat account or refreshes an existing one. Without `user_id` + `token`
/// (a personal access token), opens the server's login page.
#[tauri::command(rename_all = "snake_case")]
pub async fn rocketchat_sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    server: String,
    user_id: Option<String>,
    token: Option<String>,
    account_id: Option<String>,
) -> CmdResult<SignInDto> {
    use agora_rocketchat as rc;
    let info = rc::probe_server(&server).await?;
    let given = user_id.zip(token).map(|(u, t)| (u.trim().to_string(), t.trim().to_string()));
    let creds = match given.filter(|(u, t)| !u.is_empty() && !t.is_empty()) {
        Some((user_id, token)) => rc::Credentials { user_id, token },
        None => {
            let title = format!("Sign in to {}", info.site_name);
            let start = format!("{}/home", info.url);
            let sets: &[&[&str]] = &[&[rc::COOKIE_UID, rc::COOKIE_TOKEN], &[rc::COOKIE_UID_COPY, rc::COOKIE_TOKEN_COPY]];
            let mut v = login::capture_cookies(&app, &title, &start, sets, Some(rc::SIGNIN_SCRIPT)).await?;
            let decode = |s: String| s.replace("%2B", "+").replace("%2F", "/").replace("%3D", "=");
            rc::Credentials { token: decode(v.remove(1)), user_id: decode(v.remove(0)) }
        }
    };
    let who = rc::whoami(&info.url, &creds).await?;
    let name = if info.site_name != "Rocket.Chat" { info.site_name.clone() } else { host(&info.url) };
    let signed = SignedIn { kind: ServiceKind::RocketChat, server: info.url, user_id: who.user_id, username: who.username, name, team_id: None };
    save_sign_in(&state, signed, &creds.to_json(), account_id)
}

/// Adds every Slack workspace signed in to in the login window (one account each), or
/// refreshes `account_id`. `workspace` (optional) is where the window starts; `token` +
/// `cookie` (the web client's `xoxc-` token and `d` cookie) skip the window.
#[tauri::command(rename_all = "snake_case")]
pub async fn slack_sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    server: Option<String>,
    token: Option<String>,
    cookie: Option<String>,
    account_id: Option<String>,
) -> CmdResult<Vec<SignInDto>> {
    use agora_slack as sl;
    let given = token.zip(cookie).filter(|(t, c)| !t.trim().is_empty() && !c.trim().is_empty());
    let logins: Vec<sl::Credentials> = match given {
        Some((t, c)) => vec![sl::Credentials::new(&t, &c)],
        None => {
            let typed = server.as_deref().unwrap_or_default();
            let given_ws = sl::workspace_url(typed);
            // Adding a workspace (not re-signing in): the Slack session Agora already holds is
            // usually signed in to it too, which needs no window at all.
            if let (Some(ws), None) = (&given_ws, &account_id) {
                if let Some(creds) = reuse_slack_session(&state, ws).await {
                    let who = sl::whoami(&creds).await?;
                    let signed = SignedIn {
                        kind: ServiceKind::Slack,
                        server: who.url,
                        user_id: who.user_id,
                        username: who.username,
                        name: who.team,
                        team_id: Some(who.team_id),
                    };
                    return Ok(vec![save_sign_in(&state, signed, &creds.to_json(), None)?]);
                }
            }
            // Always Slack's general page (email, Google, Apple, workspace list). A workspace's own
            // address can jump straight to its single sign-on (e.g. a university login).
            let start: tauri::Url = sl::SIGNIN_URL.parse().map_err(err)?;
            let cookies_at: tauri::Url = sl::CLIENT_URL.parse().map_err(err)?;
            let window = login::open(&app, "Sign in to Slack", &start, Some(sl::SIGNIN_SCRIPT))?;
            let mut tick = 0u32;
            login::poll(&app, window, |w| {
                tick += 1;
                let (given_ws, cookies_at) = (given_ws.clone(), cookies_at.clone());
                async move {
                    let d = login::cookie(&w, &cookies_at, sl::COOKIE_D)?;
                    // Every workspace, once the Slack client has started in the window.
                    if let Some(raw) = login::cookie(&w, &cookies_at, sl::TEAMS_COOKIE) {
                        let all: Vec<_> = sl::parse_teams(&raw).iter().map(|t| sl::Credentials::new(&t.token, &d)).collect();
                        if !all.is_empty() {
                            return Some(all);
                        }
                    }
                    // Faster: as soon as the login is done, read the token of every workspace seen
                    // (typed, the page the window is on, the list after an email sign-in) instead
                    // of waiting for the full client. Every ~2 s.
                    if tick % 3 != 0 {
                        return None;
                    }
                    let mut candidates: Vec<String> = given_ws.into_iter().collect();
                    candidates.extend(w.url().ok().and_then(|u| sl::workspace_of(u.as_str())));
                    if let Some(raw) = login::cookie(&w, &cookies_at, sl::WORKSPACES_COOKIE) {
                        candidates.extend(sl::parse_workspaces(&raw));
                    }
                    candidates.sort();
                    candidates.dedup();
                    let mut found = Vec::new();
                    for ws in candidates {
                        if let Some(token) = sl::token_from_cookie(&ws, &d).await {
                            if !found.iter().any(|c: &sl::Credentials| c.token == token) {
                                found.push(sl::Credentials::new(&token, &d));
                            }
                        }
                    }
                    (!found.is_empty()).then_some(found)
                }
            })
            .await?
        }
    };
    // On a re-sign-in, only the account asked for is refreshed; no workspaces are added.
    let wanted_team = match &account_id {
        Some(id) => state.engine.accounts().into_iter().find(|a| &a.id == id).and_then(|a| a.team_id),
        None => None,
    };
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for creds in logins {
        let who = match sl::whoami(&creds).await {
            Ok(w) => w,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        if account_id.is_some() && wanted_team.as_deref().is_some_and(|t| t != who.team_id) {
            continue;
        }
        let signed = SignedIn {
            kind: ServiceKind::Slack,
            server: who.url,
            user_id: who.user_id,
            username: who.username,
            name: who.team,
            team_id: Some(who.team_id),
        };
        out.push(save_sign_in(&state, signed, &creds.to_json(), account_id.clone())?);
    }
    if out.is_empty() {
        return Err(errors.pop().unwrap_or_else(|| "No Slack workspace found in that session".into()));
    }
    Ok(out)
}

/// Tries the `d` cookie of every Slack account already added on workspace `ws`;
/// returns that workspace's credentials when one of those sessions is signed in to it.
async fn reuse_slack_session(state: &AppState, ws: &str) -> Option<agora_slack::Credentials> {
    let mut cookies: Vec<String> = Vec::new();
    for a in state.engine.accounts().into_iter().filter(|a| a.kind == ServiceKind::Slack) {
        let stored = secrets::get(&secrets::account_key(&a.id)).ok().flatten();
        if let Some(c) = stored.as_deref().and_then(agora_slack::Credentials::parse) {
            if !cookies.contains(&c.cookie) {
                cookies.push(c.cookie);
            }
        }
    }
    for cookie in cookies {
        if let Some(token) = agora_slack::token_from_cookie(ws, &cookie).await {
            return Some(agora_slack::Credentials::new(&token, &cookie));
        }
    }
    None
}

struct SignedIn {
    kind: ServiceKind,
    server: String,
    user_id: String,
    username: String,
    /// Account name for a new account.
    name: String,
    /// Slack workspace id.
    team_id: Option<String>,
}

/// Stores the credentials: refreshes `account_id` (or the same user on the same server),
/// otherwise adds a new account.
fn save_sign_in(state: &AppState, who: SignedIn, secret: &str, account_id: Option<String>) -> CmdResult<SignInDto> {
    let accounts = state.engine.accounts();
    let existing = accounts.iter().find(|a| match &account_id {
        Some(id) => &a.id == id,
        None => a.kind == who.kind && a.server_url == who.server && a.self_user_id.as_deref() == Some(who.user_id.as_str()),
    });
    let account = match existing {
        Some(a) => {
            if a.self_user_id.as_deref().is_some_and(|u| u != who.user_id) {
                return Err(format!("You signed in as @{}, but this account belongs to another user", who.username));
            }
            secrets::set(&secrets::account_key(&a.id), secret)?;
            if a.enabled {
                state.engine.restart_account(&a.id)?;
            }
            a.clone()
        }
        None => {
            let mut account = Account::new(who.kind, &who.name, &who.server, accounts.len() as i64);
            account.self_user_id = Some(who.user_id);
            account.team_id = who.team_id;
            secrets::set(&secrets::account_key(&account.id), secret)?;
            state.engine.add_account(account.clone())?;
            account
        }
    };
    Ok(SignInDto { account, username: who.username })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn update_account(state: State<'_, AppState>, account: Account) -> CmdResult<()> {
    state.engine.update_account(account)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn reorder_accounts(state: State<'_, AppState>, ids: Vec<String>) -> CmdResult<()> {
    state.engine.reorder_accounts(&ids)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remove_account(state: State<'_, AppState>, account_id: String, purge: bool) -> CmdResult<()> {
    state.engine.remove_account(&account_id, purge)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn reconnect(state: State<'_, AppState>) -> CmdResult<()> {
    state.engine.reconnect_all();
    Ok(())
}

/// "Reload" in a workspace's menu: drops its connection and connects again.
#[tauri::command(rename_all = "snake_case")]
pub async fn reconnect_account(state: State<'_, AppState>, account_id: String) -> CmdResult<()> {
    state.engine.restart_account(&account_id)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn home(state: State<'_, AppState>) -> CmdResult<Vec<HomeItem>> {
    state.engine.home()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn find_users(state: State<'_, AppState>, account_id: String, query: String) -> CmdResult<Vec<User>> {
    state.engine.find_users(&account_id, &query, 8)
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%' && i + 2 < b.len())
            .then(|| std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()))
            .flatten();
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Sends a file to a conversation. The bytes travel as the raw request body (no JSON
/// array of numbers); account, channel, file name and thread come as URL-encoded headers.
#[tauri::command]
pub async fn upload_file(request: tauri::ipc::Request<'_>, state: State<'_, AppState>) -> CmdResult<()> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file bytes".into());
    };
    let header = |k: &str| request.headers().get(k).and_then(|v| v.to_str().ok()).map(pct_decode).filter(|v| !v.is_empty());
    let account = header("x-account").ok_or("missing account")?;
    let channel = header("x-channel").ok_or("missing conversation")?;
    let name = header("x-name").unwrap_or_else(|| "image.png".into());
    let thread = header("x-thread");
    state.engine.upload(&account, &channel, &name, bytes.clone(), thread.as_deref()).await
}

/// Text size of the main window as a zoom: everything (sidebar, chat, settings) scales together,
/// and positions stay consistent, unlike a CSS zoom.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_zoom(app: AppHandle, scale: f64) -> CmdResult<()> {
    let w = app.get_webview_window("main").ok_or("no main window")?;
    w.set_zoom(scale.clamp(0.8, 1.5)).map_err(err)
}

/// Paints the Windows title bar like the app's top edge: `dark` picks the light or dark frame
/// (Windows 10 and 11), `caption`/`border` are "#rrggbb" colours that Windows 11 applies on
/// top; older versions ignore them and keep the plain light/dark frame.
///
/// The window keeps its title ("Agora", shown in the taskbar preview and Alt+Tab), but the title
/// bar does not draw it: the caption text is turned off, and on Windows 11 its colour is also
/// the caption colour. `text` is unused for that reason (kept so the UI call stays the same).
#[tauri::command(rename_all = "snake_case")]
pub async fn set_titlebar(app: AppHandle, dark: bool, caption: String, text: String, border: String) -> CmdResult<()> {
    let _ = text;
    let w = app.get_webview_window("main").ok_or("no main window")?;
    w.set_theme(Some(if dark { tauri::Theme::Dark } else { tauri::Theme::Light })).map_err(err)?;
    #[cfg(windows)]
    {
        #[link(name = "dwmapi")]
        extern "system" {
            fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const u32, size: u32) -> i32;
        }
        #[repr(C)]
        struct WtaOptions {
            flags: u32,
            mask: u32,
        }
        #[link(name = "uxtheme")]
        extern "system" {
            fn SetWindowThemeAttribute(hwnd: isize, attr: u32, value: *const WtaOptions, size: u32) -> i32;
        }
        // WTA_NONCLIENT = 1, WTNCA_NODRAWCAPTION = 1: no title text in the frame.
        let no_caption = WtaOptions { flags: 1, mask: 1 };
        unsafe { SetWindowThemeAttribute(w.hwnd().map_err(err)?.0 as isize, 1, &no_caption, std::mem::size_of::<WtaOptions>() as u32) };
        // COLORREF is 0x00BBGGRR.
        let colorref = |hex: &str| -> Option<u32> {
            let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).ok().filter(|_| hex.len() == 7)?;
            Some(((v & 0xff) << 16) | (v & 0xff00) | (v >> 16))
        };
        let hwnd = w.hwnd().map_err(err)?.0 as isize;
        // DWMWA_BORDER_COLOR 34, DWMWA_CAPTION_COLOR 35, DWMWA_TEXT_COLOR 36 (= caption: hidden).
        for (attr, hex) in [(34, &border), (35, &caption), (36, &caption)] {
            if let Some(c) = colorref(hex) {
                unsafe { DwmSetWindowAttribute(hwnd, attr, &c, 4) };
            }
        }
    }
    Ok(())
}

/// Opens http(s) links in the default browser, never inside the app.
#[tauri::command(rename_all = "snake_case")]
pub async fn open_url(url: String) -> CmdResult<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only http(s) links can be opened".into());
    }
    std::process::Command::new("rundll32")
        .arg("url.dll,FileProtocolHandler")
        .arg(&url)
        .spawn()
        .map_err(err)?;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn popup_ready(state: State<'_, AppState>) -> CmdResult<Vec<PopupNote>> {
    Ok(state.popups.take_pending())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn popup_resize(app: AppHandle, height: f64) -> CmdResult<()> {
    popup::resize(&app, height).map_err(err)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn popup_closed(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    state.popups.closed(&app);
    Ok(())
}

#[derive(Clone, Serialize)]
struct OpenTarget {
    account_id: String,
    channel_id: String,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn popup_open(app: AppHandle, account_id: String, channel_id: String) -> CmdResult<()> {
    tray::show_main(&app);
    app.emit_to("main", "agora-open", OpenTarget { account_id, channel_id }).map_err(err)
}
