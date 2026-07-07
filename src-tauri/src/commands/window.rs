//! Custom-titlebar window controls (minimize / maximize / close).

#[tauri::command(rename_all = "snake_case")]
pub fn window_minimize(window: tauri::Window) {
    let _ = window.minimize();
}

#[tauri::command(rename_all = "snake_case")]
pub fn window_toggle_maximize(window: tauri::Window) {
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
    } else {
        let _ = window.maximize();
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn window_close(window: tauri::Window) {
    let _ = window.close();
}
