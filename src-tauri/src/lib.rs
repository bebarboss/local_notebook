//! DocAsk — local RAG over company documents through the Gosoft Agent Gateway.

pub mod background;
pub mod chats;
pub mod commands;
pub mod config;
pub mod diag;
pub mod encoder;
pub mod embed;
pub mod errors;
pub mod gateway_embed;
pub mod embed_server;
pub mod gateway_test;
pub mod ingest;
pub mod knowledge;
pub mod followup;
pub mod team;
pub mod connect;
pub mod llm;
pub mod redact;
pub mod rerank;
pub mod search;
pub mod secrets;
pub mod services;
pub mod share;
pub mod store;
pub mod updater;

use commands::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // a second launch (desktop icon while DocAsk sits in the tray) brings this one forward
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            background::show(app);
            // a docask:// setup link clicked while DocAsk is already running
            if let Some(link) = connect::from_args(args) {
                if let Err(e) = connect::receive(app, &link) {
                    let _ = tauri::Emitter::emit(app, "connect-error", e.message);
                }
            }
        }))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec![background::AUTOSTART_ARG])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .on_window_event(background::on_window_event)
        .setup(|app| {
            let handle = app.handle().clone();
            let state = AppState::new(&handle).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
            init_resources(app.handle());
            diag::log(format!("app start v{}", env!("CARGO_PKG_VERSION")));
            connect::register_scheme();
            // started by clicking a docask:// setup link
            if let Some(link) = connect::from_args(std::env::args()) {
                if let Err(e) = connect::receive(app.handle(), &link) {
                    log::warn!("setup link rejected: {}", e.code);
                }
            }
            ingest::ocr::set_enabled(state.settings.lock().ocr_enabled);
            ingest::set_keep_images(state.settings.lock().keep_images);
            ingest::set_load(&state.settings.lock().index_load);
            app.manage(state);
            // after manage(): it reads the app state
            team::migrate_global_switch(app.handle());
            if let Err(e) = background::init(app.handle()) {
                log::warn!("tray icon unavailable: {e}");
            }
            // the window is created hidden: show it, unless Windows started us at sign-in and
            // the user wants DocAsk to wait in the tray
            let autostarted = std::env::args().any(|a| a == background::AUTOSTART_ARG);
            let start_hidden = app.state::<AppState>().settings.lock().start_hidden;
            if autostarted && start_hidden {
                background::start_in_tray(app.handle());
            } else if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }

            // watch every service folder and catch up on changes made while closed
            let st = app.state::<AppState>();
            let services = st.services.lock().list();
            for s in &services {
                st.watchers.watch(s);
            }
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let st = h.state::<AppState>();
                for s in services {
                    st.indexer.start(h.clone(), s.id, false);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_settings,
            commands::get_autostart,
            commands::knowledge_info,
            commands::draft_knowledge,
            commands::save_knowledge,
            commands::open_knowledge_folder,
            commands::set_autostart,
            commands::save_settings,
            commands::get_gateway,
            commands::set_gateway_url,
            commands::reset_gateway_url,
            commands::save_api_key,
            commands::remove_provider,
            commands::check_gateway,
            commands::gateway_status,
            commands::list_models,
            commands::save_models,
            commands::reset_models,
            commands::import_vscode_models,
            commands::test_models,
            commands::list_services,
            commands::service_stats,
            commands::create_service,
            commands::update_service,
            commands::delete_service,
            commands::list_documents,
            commands::add_documents,
            commands::remove_document,
            commands::reindex_service,
            commands::reindex_all,
            commands::cancel_index,
            commands::export_service_zip,
            commands::import_service_zip,
            commands::move_service_folder,
            commands::list_chats,
            commands::get_chat,
            commands::get_messages,
            commands::rename_chat,
            commands::delete_chat,
            commands::delete_all_chats,
            commands::ask,
            commands::stop_generation,
            commands::open_source_file,
            commands::embedding_status,
            commands::download_embedding,
            commands::use_embedding_folder,
            commands::gateway_embedding_models,
            commands::test_gateway_embedding,
            commands::use_gateway_embedding,
            commands::embed_server_info,
            commands::set_embed_server,
            commands::test_embed_server,
            commands::use_embed_server,
            commands::forget_embed_server,
            commands::team_status,
            commands::share_service,
            commands::service_invite,
            commands::rotate_service_key,
            commands::leave_service,
            commands::team_sync_now,
            commands::connect_pending,
            commands::connect_paste,
            commands::connect_apply,
            commands::connect_discard,
            commands::wipe_all_data,
            commands::ocr_status,
            commands::rerank_status,
            commands::download_reranker,
            commands::use_reranker_folder,
            commands::restore_document,
            commands::source_images,
            commands::image_data,
            updater::check_update,
            updater::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DocAsk");
}

/// Find the bundled OCR models and pdfium.dll: installed resource dir first,
/// then `src-tauri/resources` when running from source.
fn init_resources(app: &tauri::AppHandle) {
    let mut roots = Vec::new();
    if let Ok(r) = app.path().resource_dir() {
        roots.push(r.join("resources"));
        roots.push(r);
    }
    roots.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources"));
    if let Some(root) = roots.iter().find(|r| r.join("ocr").join("rec.onnx").is_file()) {
        ingest::ocr::init(root.join("ocr"));
    }
    let dll = roots
        .iter()
        .flat_map(|r| [r.join("pdfium.dll"), r.join("pdfium").join("pdfium.dll")])
        .find(|p| p.is_file());
    if let Some(dll) = dll {
        ingest::pdf::init(dll);
    }
}
