#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod boot;
mod commands;
mod factory;
mod frontend;
mod login;
mod media;
mod migrate;
mod popup;
mod tray;

use agora_core::{engine::Engine, secrets, store::Store};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Manager, WindowEvent};

pub struct AppState {
    pub engine: Arc<Engine>,
    pub boot: Mutex<boot::BootConfig>,
    pub data_dir: PathBuf,
    pub popups: Arc<popup::Popups>,
}

fn main() {
    tracing_subscriber::fmt().with_target(false).init();

    tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol(media::SCHEME, media::handle)
        .register_asynchronous_uri_scheme_protocol(media::LEGACY_SCHEME, media::handle)
        .setup(|app| {
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
            let popups = Arc::new(popup::Popups::default());
            let frontend = Arc::new(frontend::TauriFrontend::new(app.handle().clone(), popups.clone()));
            let engine = Engine::new(store, frontend, Arc::new(factory::Factory));
            let starter = engine.clone();
            tauri::async_runtime::spawn(async move { starter.start() });

            app.manage(AppState { engine, boot: Mutex::new(boot), data_dir, popups });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                // Closing the window keeps Agora running in the tray.
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
            commands::list_accounts,
            commands::list_channels,
            commands::get_messages,
            commands::get_thread,
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
            commands::find_users,
            commands::upload_file,
            commands::open_url,
            commands::set_zoom,
            commands::set_titlebar,
            commands::get_profile,
            commands::channel_members,
            commands::popup_ready,
            commands::popup_resize,
            commands::popup_closed,
            commands::popup_open,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agora");
}
