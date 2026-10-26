// Without this, release builds on Windows also open a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod ai;
mod background;
mod calendar;
mod chime;
mod demo;
mod files;
mod live;
mod mailbox;
mod meetings;
mod notify;
mod prefs;
mod secrets;
mod signatures;
mod updates;

use tauri::Manager;

fn main() {
    // From the uninstaller, when the user asks to delete their data too.
    if std::env::args().any(|arg| arg == "--forget-saved-passwords") {
        if let Err(error) = secrets::forget_all() {
            eprintln!("spark-rs: couldn't remove saved passwords: {error}");
            std::process::exit(1);
        }
        return;
    }
    tauri::Builder::default()
        // First, so a second launch only brings this one's window forward.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            background::second_launch(app, args)
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            files::clear_opened();
            let config_dir = app.path().app_config_dir()?;
            app.manage(prefs::PrefsState::load(config_dir.clone()));
            app.manage(meetings::Meetings::default());
            app.manage(ai::AiState::new(config_dir, ai::Keys::platform()));
            notify::register(app.handle());
            let live = live::Live::open(&app.path().app_local_data_dir()?)?;
            live.start_all(app.handle());
            app.manage(mailbox::Mailbox {
                demo: demo::DemoMailbox::load(),
                live,
            });
            if let Err(e) = agents::start(app.handle()) {
                eprintln!("spark-rs: agents can't connect: {e}");
            }
            background::setup(app)?;
            meetings::start(app.handle());
            app.manage(updates::Updates::default());
            updates::start(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            background::on_window_event(window, event);
            meetings::on_window_event(window, event);
        })
        .invoke_handler(tauri::generate_handler![
            mailbox::list_accounts,
            mailbox::list_messages,
            mailbox::search_messages,
            mailbox::search_servers,
            mailbox::suggest_contacts,
            mailbox::never_suggest_contact,
            mailbox::list_folders,
            mailbox::watch_folders,
            mailbox::sync_folders,
            mailbox::refresh_folders,
            mailbox::load_older,
            mailbox::create_folder,
            mailbox::rename_folder,
            mailbox::delete_folder,
            mailbox::delete_forever,
            mailbox::set_muted,
            mailbox::unsubscribe,
            mailbox::empty_folders,
            mailbox::get_message_content,
            mailbox::answer_invite,
            calendar::list_calendars,
            calendar::list_events,
            calendar::refresh_calendars,
            calendar::answer_event,
            calendar::reconnect_account,
            meetings::meeting_popup_meetings,
            meetings::meeting_popup_act,
            meetings::fit_meeting_popup,
            meetings::preview_meeting_popup,
            mailbox::update_message,
            mailbox::decide_senders,
            mailbox::sender_status,
            mailbox::gatekeeper_mode,
            mailbox::set_gatekeeper_mode,
            mailbox::add_gmail_account,
            mailbox::add_imap_account,
            mailbox::trust_certificate,
            mailbox::add_microsoft_account,
            mailbox::cancel_sign_in,
            mailbox::remove_account,
            mailbox::rename_account,
            mailbox::sync_now,
            mailbox::send_message,
            mailbox::cancel_send,
            mailbox::save_draft,
            mailbox::open_draft,
            mailbox::discard_draft,
            signatures::list_signatures,
            signatures::save_signature,
            signatures::delete_signature,
            signatures::set_default_signature,
            signatures::resolve_signature,
            mailbox::open_external,
            mailbox::open_attachment,
            mailbox::save_attachment,
            mailbox::save_all_attachments,
            mailbox::inline_images,
            mailbox::pick_files,
            mailbox::describe_files,
            ai::get_ai_settings,
            ai::save_ai_settings,
            ai::test_ai_connection,
            ai::list_ai_models,
            ai::summarize_message,
            ai::cancel_summary,
            ai::compose_with_ai,
            ai::cancel_compose,
            prefs::get_prefs,
            prefs::save_prefs,
            notify::test_notification,
            background::set_badge,
            agents::get_agents,
            agents::set_agent_access,
            updates::get_updates,
            updates::check_for_updates,
            updates::install_update,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start spark-rs")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                agents::stopped(app);
            }
        });
}
