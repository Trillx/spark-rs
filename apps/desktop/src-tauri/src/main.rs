// Without this, release builds on Windows also open a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod demo;
mod live;
mod mailbox;
mod secrets;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            app.manage(ai::AiState::new(config_dir, ai::Keys::platform()));
            let live = live::Live::open(&app.path().app_local_data_dir()?)?;
            live.start_all(app.handle());
            app.manage(mailbox::Mailbox {
                demo: demo::DemoMailbox::load(),
                live,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            mailbox::list_accounts,
            mailbox::list_messages,
            mailbox::get_message_content,
            mailbox::update_message,
            mailbox::add_gmail_account,
            mailbox::add_imap_account,
            mailbox::add_microsoft_account,
            mailbox::cancel_sign_in,
            mailbox::remove_account,
            mailbox::rename_account,
            mailbox::sync_now,
            mailbox::send_message,
            mailbox::cancel_send,
            mailbox::open_external,
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
