//! The activity payload sent inside `SET_ACTIVITY`.
//!
//! Field names and types match the official activity object: `type` is the
//! numeric activity type, `timestamps.start`/`end` are Unix milliseconds, and
//! optional fields are omitted rather than sent as `null` so Discord keeps its
//! own defaults for them.

use serde::Serialize;

/// The activity type shown before the application name.
///
/// The numeric values are Discord's; only [`ActivityType::Listening`] is used by
/// Rift today, the rest are here because the type is a documented part of the
/// payload and makes the model complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityType {
    /// "Playing Rift"
    Playing,
    /// "Listening to Rift"
    Listening,
    /// "Watching Rift"
    Watching,
    /// "Competing in Rift"
    Competing,
}

impl ActivityType {
    /// The numeric representation Discord expects in the `type` field.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Playing => 0,
            Self::Listening => 2,
            Self::Watching => 3,
            Self::Competing => 5,
        }
    }
}

impl Serialize for ActivityType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u8(self.as_u8())
    }
}

/// A Rich Presence activity: the details, state, artwork, buttons and optional
/// elapsed/remaining timestamps Discord shows on a profile.
///
/// Built with the chainable setters, which take `impl Into<String>` so callers
/// can pass borrowed values from the player state without extra clones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activity {
    // Serialized as `type`, the numeric activity type.
    #[serde(rename = "type")]
    activity_type: ActivityType,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamps: Option<Timestamps>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assets: Option<Assets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    buttons: Option<Vec<Button>>,
}

impl Activity {
    /// Start an activity of `activity_type` with no optional fields set.
    pub fn new(activity_type: ActivityType) -> Self {
        Self {
            activity_type,
            details: None,
            state: None,
            timestamps: None,
            assets: None,
            buttons: None,
        }
    }

    /// A "Listening to …" activity, the type Rift uses for music.
    pub fn listening() -> Self {
        Self::new(ActivityType::Listening)
    }

    /// The first line under the application name (Rift sends the track title).
    pub fn details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// The second line under the application name (Rift sends the artist).
    pub fn state(mut self, state: impl Into<String>) -> Self {
        self.state = Some(state.into());
        self
    }

    /// The elapsed/remaining pair shown as a time bar.
    pub fn timestamps(mut self, timestamps: Timestamps) -> Self {
        self.timestamps = Some(timestamps);
        self
    }

    /// The artwork and hover texts.
    pub fn assets(mut self, assets: Assets) -> Self {
        self.assets = Some(assets);
        self
    }

    /// Up to two buttons shown under the activity. Discord's documented maximum
    /// is two; the caller is responsible for staying within it.
    pub fn buttons(mut self, buttons: Vec<Button>) -> Self {
        self.buttons = Some(buttons);
        self
    }
}

/// Unix-millisecond endpoints for Discord's elapsed/remaining time bar.
///
/// Both fields are independent; Discord renders elapsed time from `start` and
/// remaining time to `end`, so a music player normally sends both as a
/// `(track start, track end)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Timestamps {
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<i64>,
}

impl Timestamps {
    /// Empty timestamps (`SET_ACTIVITY` then carries an empty object).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set when the activity started, in Unix milliseconds.
    pub const fn start(mut self, milliseconds: i64) -> Self {
        self.start = Some(milliseconds);
        self
    }

    /// Set when the activity ends, in Unix milliseconds.
    pub const fn end(mut self, milliseconds: i64) -> Self {
        self.end = Some(milliseconds);
        self
    }

    /// The common music-player pair: `start` now, `end` at `start + duration`.
    pub const fn start_end(start_milliseconds: i64, end_milliseconds: i64) -> Self {
        Self {
            start: Some(start_milliseconds),
            end: Some(end_milliseconds),
        }
    }

    /// Whether neither endpoint is set (the bar will not render).
    pub const fn is_empty(&self) -> bool {
        self.start.is_none() && self.end.is_none()
    }
}

/// The art assets shown on the activity card.
///
/// Rift uses `large_image` for the track cover and `small_image`/`small_text`
/// for its own logo badge.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Assets {
    #[serde(skip_serializing_if = "Option::is_none")]
    large_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    large_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    small_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    small_text: Option<String>,
}

impl Assets {
    /// No art. Add images with the setters.
    pub fn new() -> Self {
        Self::default()
    }

    /// The large image key (Rift passes the cover URL).
    pub fn large_image(mut self, key: impl Into<String>) -> Self {
        self.large_image = Some(key.into());
        self
    }

    /// Hover text for the large image.
    pub fn large_text(mut self, text: impl Into<String>) -> Self {
        self.large_text = Some(text.into());
        self
    }

    /// The small image key (Rift's app logo asset).
    pub fn small_image(mut self, key: impl Into<String>) -> Self {
        self.small_image = Some(key.into());
        self
    }

    /// Hover text for the small image.
    pub fn small_text(mut self, text: impl Into<String>) -> Self {
        self.small_text = Some(text.into());
        self
    }
}

/// One of the two buttons Discord allows under an activity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Button {
    label: String,
    url: String,
}

impl Button {
    /// A button labelled `label` that opens `url`.
    pub fn new(label: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            url: url.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listening_activity_serializes_to_the_documented_shape() {
        let activity = Activity::listening()
            .details("Song")
            .state("Artist")
            .assets(Assets::new().large_image("https://example.test/cover.jpg"))
            .buttons(vec![Button::new("GitHub", "https://example.test/repo")])
            .timestamps(Timestamps::start_end(1_700_000_000_000, 1_700_000_200_000));

        let value = serde_json::to_value(&activity).expect("activity serializes");
        assert_eq!(value["type"], 2, "Listening is activity type 2");
        assert_eq!(value["details"], "Song");
        assert_eq!(value["state"], "Artist");
        assert_eq!(
            value["assets"]["large_image"],
            "https://example.test/cover.jpg"
        );
        assert_eq!(
            value["timestamps"],
            serde_json::json!({ "start": 1_700_000_000_000_i64, "end": 1_700_000_200_000_i64 }),
            "timestamps are Unix milliseconds"
        );
        assert_eq!(value["buttons"][0]["label"], "GitHub");
        assert_eq!(value["buttons"][0]["url"], "https://example.test/repo");
        assert!(
            value.get("large_text").is_none(),
            "unset optional fields must be omitted, not nulled"
        );
    }

    #[test]
    fn optional_fields_are_omitted_when_unset() {
        let value = serde_json::to_value(Activity::listening()).expect("serializes");
        assert_eq!(value, serde_json::json!({ "type": 2 }));
        assert!(Timestamps::new().is_empty());
        assert!(!Timestamps::start_end(1, 2).is_empty());
    }

    #[test]
    fn the_other_activity_types_serialize_to_their_numeric_values() {
        for (activity_type, expected) in [
            (ActivityType::Playing, 0),
            (ActivityType::Listening, 2),
            (ActivityType::Watching, 3),
            (ActivityType::Competing, 5),
        ] {
            let value = serde_json::to_value(Activity::new(activity_type)).expect("serializes");
            assert_eq!(value["type"], expected);
        }
    }
}
