// Without this, release builds on Windows also open a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod mailbox;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .manage(mailbox::DemoMailbox::load())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            app.manage(ai::AiState::new(config_dir, ai::Keys::platform()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            mailbox::list_messages,
            mailbox::update_message,
            ai::get_ai_settings,
            ai::save_ai_settings,
            ai::test_ai_connection,
            ai::list_ai_models,
            ai::summarize_message,
            ai::cancel_summary,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start spark-rs");
}
