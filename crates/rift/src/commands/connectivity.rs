//! Network reachability, exposed to the app.
//!
//! The old webview had `online`/`offline` listeners; the native app has no
//! browser to inherit that signal from, so the controller polls this instead
//! (~every 10 s, from a task) and greys out un-downloaded rows while offline.

use crate::backend::BackendHandle;

impl BackendHandle {
    /// Cheap network reachability probe: `HEAD https://www.youtube.com/` with a
    /// 2 s cap, using the backend's shared `reqwest::Client`.
    ///
    /// `true` when the request reached a `youtube.com` host — **any** HTTP status
    /// counts (a 403/429 from `YouTube`'s own edge while rate-limited is still
    /// online); `false` only on transport failure or when the response did not
    /// come from `YouTube` (captive portal / transparent proxy). The same rule the
    /// download-retry path uses (see `util::is_online`), so "offline" means the
    /// same thing everywhere.
    ///
    /// **Async, network I/O:** poll it from a spawned task, never on the UI
    /// thread. It emits no events; the caller owns the resulting state (the app
    /// controller turns it into `ControllerEvent::Online(bool)`).
    pub async fn is_online(&self) -> bool {
        crate::util::is_online(&self.inner.player.http).await
    }
}
