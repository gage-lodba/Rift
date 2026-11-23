//! Controller-side cover request de-duplication; decoding and the tracker live in [`crate::cover`].

use super::{Controller, ControllerEvent};
use crate::cover::CoverRequest;

impl Controller {
    /// Ask the backend to fetch and decode a cover-art thumbnail.
    ///
    /// De-duplicated: at most one fetch per URL is in flight and a URL that
    /// already failed is never retried (the UI renders the fallback tile). In
    /// preview mode there is no HTTP client, so the URL fails immediately and the
    /// UI shows the fallback instead of waiting forever.
    pub fn request_cover(&mut self, url: String) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        if !self.live {
            self.pending.push(ControllerEvent::CoverFailed(url));
            return;
        }
        match self.covers.request(&url) {
            CoverRequest::Fetch => self.commands.fetch_cover(url),
            CoverRequest::InFlight => {}
            CoverRequest::Failed => self.pending.push(ControllerEvent::CoverFailed(url)),
        }
    }
}
