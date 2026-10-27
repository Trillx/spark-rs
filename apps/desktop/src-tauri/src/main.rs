// Without this, release builds on Windows also open a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod ai;
mod alerts;
mod background;
mod calendar;
mod chime;
mod demo;
mod files;
mod live;
mod mailbox;
mod mailto;
mod meetings;
mod notify;
mod prefs;
mod secrets;
mod signatures;
mod templates;
mod updates;

use tauri::Manager;

/// The uninstaller asks to delete saved passwords too, when the user
/// deletes their data: that flag alone. An email link's launch carries
/// arguments whoever made the link wrote (Windows passes it on unescaped), so
/// the flag among others doesn't count.
fn forget_requested(args: &[String]) -> bool {
    matches!(args, [_, flag] if flag == "--forget-saved-passwords")
}

fn main() {
    if forget_requested(&std::env::args().collect::<Vec<_>>()) {
        if let Err(error) = secrets::forget_all() {
            eprintln!("spark-rs: couldn't remove saved passwords: {error}");
            std::process::exit(1);
        }
        return;
    }
    tauri::Builder::default()
        .manage(mailto::Pending::default())
        // First, so a second launch only brings this one's window forward,
        // with the email link it was given, if any.
        .plugin(tauri_plugin_single_instance::init(
            |app, args, _cwd| match mailto::link_in(&args) {
                Some(link) => mailto::received(app, link),
                None => background::second_launch(app, args),
            },
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            files::clear_opened();
            mailto::started_with(app.handle());
            mailto::refresh();
            let config_dir = app.path().app_config_dir()?;
            app.manage(prefs::PrefsState::load(config_dir.clone()));
            app.manage(meetings::Meetings::default());
            app.manage(alerts::Alerts::default());
            app.manage(ai::AiState::new(config_dir.clone(), ai::Keys::platform()));
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
            alerts::start(app.handle(), &config_dir);
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
            mailbox::set_sender_category,
            mailbox::list_scheduled,
            mailbox::reschedule_send,
            mailbox::unschedule_send,
            mailbox::list_reminders,
            mailbox::set_reminder,
            mailbox::finish_reminder,
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
            mailbox::priority_senders,
            mailbox::prioritize_senders,
            mailbox::undo_prioritize,
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
            templates::list_templates,
            templates::save_template,
            templates::delete_template,
            templates::order_templates,
            templates::template_used,
            mailbox::open_external,
            mailbox::open_attachment,
            mailbox::save_attachment,
            mailbox::save_all_attachments,
            mailbox::inline_images,
            mailbox::pick_files,
            mailbox::describe_files,
            mailbox::read_image,
            mailto::take_mailto,
            mailto::mail_app,
            mailto::make_default_mail_app,
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
            notify::focus_reminder,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_uninstaller_can_forget_passwords() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert!(forget_requested(&args(&[
            "spark.exe",
            "--forget-saved-passwords"
        ])));
        // A link that slips the flag in, as `"%1"` lets it.
        assert!(!forget_requested(&args(&[
            "spark.exe",
            "mailto:a@x.com",
            "--forget-saved-passwords",
            "",
        ])));
        assert!(!forget_requested(&args(&["spark.exe"])));
    }
}
