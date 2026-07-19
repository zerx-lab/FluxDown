pub(crate) mod download_actor;

pub async fn create_actors() {
    // Determine the data directory using the shared resolver.
    //
    // Linux:   $XDG_DATA_HOME/fluxdown  (~/.local/share/fluxdown)
    // macOS:   ~/Library/Application Support/fluxdown
    // Windows portable (marker file present): exe directory
    // Windows installed: %LOCALAPPDATA%\FluxDown
    let db_dir = match fluxdown_engine::data_dir::resolve_data_dir(None) {
        Ok(dir) => dir,
        Err(e) => {
            crate::logger::write_error(&format!("Failed to resolve data directory: {e}"));
            return;
        }
    };

    download_actor::run(db_dir).await;
}
