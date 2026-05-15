pub mod align;
pub mod asr;
mod commands;
pub mod ffmpeg;
pub mod models;
mod pipeline;
pub mod settings;
pub mod srt;
pub mod translate;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .manage(commands::pipeline::PipelineState::new())
        .invoke_handler(tauri::generate_handler![
            commands::models::list_models,
            commands::models::download_model,
            commands::pipeline::start_resync_fast,
            commands::pipeline::start_resync_precise,
            commands::pipeline::start_generate,
            commands::pipeline::cancel_pipeline,
            commands::settings::get_providers,
            commands::settings::save_provider,
            commands::settings::set_active_provider,
            commands::settings::test_provider,
            commands::settings::get_active_model,
            commands::settings::set_active_model,
            commands::system::system_check,
            commands::system::detect_sibling_srt,
            commands::system::list_embedded_subtitles,
            commands::system::download_ffmpeg,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
