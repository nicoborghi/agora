//! The Agora app, shared by the desktop executable (main.rs) and the Android library.

mod boot;
mod commands;
mod factory;
mod frontend;
mod media;
mod migrate;
mod storage;
// Desktop only; Android gets the stand-ins in mobile.rs.
#[cfg(desktop)]
mod login;
#[cfg(mobile)]
mod mobile;
#[cfg(desktop)]
mod popup;
#[cfg(desktop)]
mod tray;
#[cfg(mobile)]
use mobile::{login, popup, tray};
#[cfg(target_os = "android")]
mod android;

use agora_core::{engine::Engine, secrets, store::Store};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Manager, WindowEvent};

pub struct AppState {
    pub engine: Arc<Engine>,
    pub boot: Mutex<boot::BootConfig>,
    pub data_dir: PathBuf,
    pub popups: Arc<popup::Popups>,
    /// An update found by `check_update`, waiting for the user to install it.
    #[cfg(desktop)]
    pub update: Mutex<Option<tauri_plugin_updater::Update>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(not(target_os = "android"))]
    tracing_subscriber::fmt().with_target(false).init();
    // Android drops stdout: the log goes to a file in the app's private folder instead
    // (`adb shell run-as app.agora.mobile cat agora.log`), started afresh each launch.
    #[cfg(target_os = "android")]
    if let Ok(f) = std::fs::File::create("/data/data/app.agora.mobile/agora.log") {
        tracing_subscriber::fmt().with_target(false).with_ansi(false).with_writer(std::sync::Mutex::new(f)).init();
    }
    // Before Tauri opens the window, whose webview then holds its cache open.
    #[cfg(windows)]
    storage::clear_webview_if_asked("app.agora.desktop");

    let builder = tauri::Builder::default();
    // First, so a second start stops here and only shows the window of the first. Installed
    // builds only: a development build must start even with a hidden copy in the tray (an
    // earlier `npm run dev` or the installed Agora), or it would show that copy and quit.
    #[cfg(all(desktop, not(debug_assertions)))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_main(app)));
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec![commands::HIDDEN_ARG])))
        .plugin(tauri_plugin_updater::Builder::new().build());
    // Sign-in pages open in the app's own webview on Android: web links only, so `slack:` or
    // `intent:` links cannot hand the sign-in to the service's app instead.
    #[cfg(mobile)]
    let builder = builder.plugin(
        tauri::plugin::Builder::<tauri::Wry>::new("agora-links")
            .on_navigation(|_, url| {
                let web = matches!(url.scheme(), "https" | "http" | "about" | "data" | "blob" | "tauri" | "ipc" | "asset");
                if !web {
                    tracing::info!("blocked a {}: link", url.scheme());
                }
                web
            })
            .build(),
    );
    builder
        .register_asynchronous_uri_scheme_protocol(media::SCHEME, media::handle)
        .register_asynchronous_uri_scheme_protocol(media::LEGACY_SCHEME, media::handle)
        .setup(|app| {
            #[cfg(target_os = "android")]
            agora_core::secrets::install(Box::new(android::KeystoreSecrets));
            let data_dir = app.path().app_data_dir()?;
            migrate::legacy_folder(&data_dir);
            if let Ok(local) = app.path().app_local_data_dir() {
                migrate::legacy_folder(&local);
            }
            std::fs::create_dir_all(&data_dir)?;
            migrate::legacy_files(&data_dir);
            let boot = boot::BootConfig::load(&data_dir);
            boot.save(&data_dir)?;
            let key = if boot.db_encrypted { Some(secrets::db_key().map_err(std::io::Error::other)?) } else { None };
            let store = Arc::new(Store::open(&data_dir.join("agora.db"), key.as_deref())?);

            tray::create(app.handle())?;
            let handle = app.handle().clone();
            std::thread::spawn(move || commands::prune_files(&handle));
            let popups = Arc::new(popup::Popups::default());
            let frontend = Arc::new(frontend::TauriFrontend::new(app.handle().clone(), popups.clone()));
            let engine = Engine::new(store, frontend, Arc::new(factory::Factory));
            let starter = engine.clone();
            tauri::async_runtime::spawn(async move { starter.start() });
            commands::apply_autostart(app.handle(), engine.settings().autostart);
            // Started at login: straight to the tray.
            #[cfg(desktop)]
            if std::env::args().any(|a| a == commands::HIDDEN_ARG) {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            app.manage(AppState {
                engine,
                boot: Mutex::new(boot),
                data_dir,
                popups,
                #[cfg(desktop)]
                update: Mutex::new(None),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                // Closing the window keeps Agora running in the tray.
                #[cfg(desktop)]
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                    if let Some(state) = window.try_state::<AppState>() {
                        state.engine.set_focused(false);
                    }
                }
                WindowEvent::Focused(focused) => {
                    if let Some(state) = window.try_state::<AppState>() {
                        state.engine.set_focused(*focused);
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::set_foreground,
            commands::outbox,
            commands::cancel_queued,
            commands::app_lock,
            commands::qr_svg,
            commands::set_app_lock,
            commands::list_accounts,
            commands::list_channels,
            commands::get_messages,
            commands::get_thread,
            commands::channel_images,
            commands::open_channel,
            commands::send_message,
            commands::edit_message,
            commands::delete_message,
            commands::react,
            commands::preview_remote,
            commands::set_starred,
            commands::set_local_group,
            commands::search_messages,
            commands::get_me,
            commands::set_presence,
            commands::use_service_icon,
            commands::get_channel_prefs,
            commands::set_channel_prefs,
            commands::save_settings,
            commands::set_db_encryption,
            commands::add_demo_account,
            commands::mattermost_sign_in,
            commands::rocketchat_sign_in,
            commands::slack_sign_in,
            commands::update_account,
            commands::reorder_accounts,
            commands::remove_account,
            commands::reconnect,
            commands::reconnect_account,
            commands::home,
            commands::catch_up,
            commands::storage_usage,
            commands::free_storage,
            commands::mark_conversations_read,
            commands::find_users,
            commands::upload_file,
            commands::open_url,
            commands::open_attachment,
            commands::read_file,
            commands::set_zoom,
            commands::set_titlebar,
            commands::get_profile,
            commands::get_bookmarks,
            commands::check_update,
            commands::install_update,
            commands::set_bookmark,
            commands::get_todos,
            commands::add_todo,
            commands::add_own_todo,
            commands::remove_todo,
            commands::update_todo,
            commands::channel_members,
            commands::channel_details,
            commands::get_canvas,
            commands::thread_opened,
            commands::message_link,
            commands::open_dm,
            commands::mark_unread,
            commands::custom_emoji,
            commands::popup_ready,
            commands::popup_resize,
            commands::popup_closed,
            commands::popup_open,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agora");
}
