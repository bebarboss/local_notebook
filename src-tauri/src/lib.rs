//! DocAsk — local RAG over company documents through the Gosoft Agent Gateway.

pub mod chats;
pub mod commands;
pub mod config;
pub mod embed;
pub mod errors;
pub mod gateway_test;
pub mod ingest;
pub mod llm;
pub mod redact;
pub mod search;
pub mod secrets;
pub mod services;
pub mod store;
pub mod updater;

use commands::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = AppState::new(&handle).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
            init_resources(app.handle());
            ingest::ocr::set_enabled(state.settings.lock().ocr_enabled);
            app.manage(state);

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
            commands::export_service,
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
            commands::wipe_all_data,
            commands::ocr_status,
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
