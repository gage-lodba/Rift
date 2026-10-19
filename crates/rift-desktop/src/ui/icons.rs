//! The UI's icon set.
//!
//! Every icon is a glyph in the embedded Material Icons fonts (see
//! `crates/rift-desktop/src/text/`), addressed by Unicode private-use codepoint. The old web UI
//! used inline *filled* Material SVG paths (`fill="currentColor"`), so filled is
//! the default rendering; [`IconFont::Outlined`] is available when a screen wants
//! the lighter outline variant (both fonts are embedded, same codepoints).
//!
//! `Icon::Heart` is the **outline** heart (`favorite_border`); `Icon::HeartFilled`
//! is the solid one (`favorite`). Both are glyphs of the filled font, matching
//! the old UI's `heart-outline`/`heart` distinction.

/// Which embedded icon font to rasterise from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconFont {
    /// Filled Material Icons — the old UI's inline SVG style.
    #[default]
    Filled,
    /// Outlined Material Icons.
    Outlined,
}

/// A named icon. Codepoints are from the Material Icons codepoint map
/// (`crates/rift-desktop/assets/fonts/MaterialIcons-Regular.codepoints`); the
/// `every_ui_icon_codepoint_exists_in_both_icon_fonts` unit test keeps them in
/// sync with the embedded fonts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(dead_code)] // the full set is published for screens; not all are used yet
pub enum Icon {
    // transport / player
    Play,
    Pause,
    SkipNext,
    SkipPrev,
    Shuffle,
    Repeat,
    RepeatOne,
    PlayCircle,
    VolumeUp,
    VolumeDown,
    VolumeOff,
    VolumeMute,
    // library / navigation
    Heart,
    HeartFilled,
    Like,
    Home,
    MusicNote,
    Album,
    LibraryMusic,
    Person,
    Queue,
    PlaylistAdd,
    PlaylistPlay,
    List,
    Radio,
    // chrome
    Search,
    Settings,
    MoreVert,
    MoreHoriz,
    DragHandle,
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    ArrowBack,
    ArrowForward,
    ExpandMore,
    ExpandLess,
    // actions
    Add,
    AddCircle,
    Delete,
    Close,
    Check,
    CheckCircle,
    Cancel,
    Edit,
    Share,
    Save,
    ContentCopy,
    Download,
    Upload,
    FileOpen,
    FolderOpen,
    OpenInNew,
    Refresh,
    Sync,
    // window controls
    Minimize,
    Maximize,
    Fullscreen,
    FullscreenExit,
    // system / status
    CloudOff,
    Offline,
    Info,
    Error,
    Warning,
    Timer,
    WifiOff,
    Mic,
    Headset,
    Lyrics,
    Speed,
    Equalizer,
    GraphicEq,
    AudioTrack,
    Image,
    Photo,
    FilterList,
    Sort,
    AccountCircle,
    Logout,
    Login,
    Notifications,
    Done,
    DoneAll,
    RadioButtonChecked,
    RadioButtonUnchecked,
    CheckBox,
    CheckBoxOutlineBlank,
}

impl Icon {
    /// The icon's Material Icons codepoint (private use area).
    pub const fn codepoint(self) -> char {
        use Icon::*;
        let cp: u32 = match self {
            Play => 0xe037,
            Pause => 0xe034,
            SkipNext => 0xe044,
            SkipPrev => 0xe045,
            Shuffle => 0xe043,
            Repeat => 0xe040,
            RepeatOne => 0xe041,
            PlayCircle => 0xe934,
            VolumeUp => 0xe050,
            VolumeDown => 0xe04d,
            VolumeOff => 0xe04f,
            VolumeMute => 0xe04e,
            Heart => 0xe87e,       // favorite_border (outline)
            HeartFilled => 0xe87d, // favorite
            Like => 0xe8dc,        // thumb_up
            Home => 0xe88a,
            MusicNote => 0xe405,
            Album => 0xe019,
            LibraryMusic => 0xe030,
            Person => 0xe7fd,
            Queue => 0xe03c,
            PlaylistAdd => 0xe03b,
            PlaylistPlay => 0xe05f,
            List => 0xe896,
            Radio => 0xe03e,
            Search => 0xe8b6,
            Settings => 0xe8b8,
            MoreVert => 0xe5d4,
            MoreHoriz => 0xe5d3,
            DragHandle => 0xe945,
            ChevronDown => 0xe5cf, // expand_more
            ChevronRight => 0xe5cc,
            ChevronLeft => 0xe5cb,
            ArrowBack => 0xe5c4,
            ArrowForward => 0xe5c8,
            ExpandMore => 0xe5cf,
            ExpandLess => 0xe5ce,
            Add => 0xe145,
            AddCircle => 0xe147,
            Delete => 0xe872,
            Close => 0xe5cd,
            Check => 0xe5ca,
            CheckCircle => 0xe86c,
            Cancel => 0xe5c9,
            Edit => 0xe3c9,
            Share => 0xe80d,
            Save => 0xe161,
            ContentCopy => 0xe14d,
            Download => 0xf090,
            Upload => 0xf09b,
            FileOpen => 0xeaf3,
            FolderOpen => 0xe2c8,
            OpenInNew => 0xe89e,
            Refresh => 0xe5d5,
            Sync => 0xe627,
            Minimize => 0xe15b, // remove
            Maximize => 0xe3c6, // crop_square
            Fullscreen => 0xe5d0,
            FullscreenExit => 0xe5d1,
            CloudOff => 0xe2c1,
            Offline => 0xe2c1,
            Info => 0xe88e,
            Error => 0xe000,
            Warning => 0xe002,
            Timer => 0xe425,
            WifiOff => 0xe648,
            Mic => 0xe029,
            Headset => 0xe310,
            Lyrics => 0xec0b,
            Speed => 0xe9e4,
            Equalizer => 0xe01d,
            GraphicEq => 0xe1b8,
            AudioTrack => 0xe3a1,
            Image => 0xe3f4,
            Photo => 0xe410,
            FilterList => 0xe152,
            Sort => 0xe164,
            AccountCircle => 0xe853,
            Logout => 0xe9ba,
            Login => 0xea77,
            Notifications => 0xe7f4,
            Done => 0xe876,
            DoneAll => 0xe877,
            RadioButtonChecked => 0xe837,
            RadioButtonUnchecked => 0xe836,
            CheckBox => 0xe834,
            CheckBoxOutlineBlank => 0xe835,
        };
        // All Material Icons codepoints are in the BMP, so this is infallible.
        match char::from_u32(cp) {
            Some(c) => c,
            None => '\u{fffd}',
        }
    }

    /// Whether this icon only exists in the filled font (solid variants).
    pub fn prefers_filled(self) -> bool {
        matches!(self, Icon::HeartFilled)
    }

    /// The font this icon is normally drawn from. All Rift icons use the
    /// filled Material Icons face; [`Ui::icon_outlined`] is the opt-in variant.
    pub fn default_font(self) -> IconFont {
        IconFont::Filled
    }

    /// Every icon, for up-front atlas prewarming.
    pub const ALL: &'static [Icon] = &[
        Icon::Play,
        Icon::Pause,
        Icon::SkipNext,
        Icon::SkipPrev,
        Icon::Shuffle,
        Icon::Repeat,
        Icon::RepeatOne,
        Icon::PlayCircle,
        Icon::VolumeUp,
        Icon::VolumeDown,
        Icon::VolumeOff,
        Icon::VolumeMute,
        Icon::Heart,
        Icon::HeartFilled,
        Icon::Like,
        Icon::Home,
        Icon::MusicNote,
        Icon::Album,
        Icon::LibraryMusic,
        Icon::Person,
        Icon::Queue,
        Icon::PlaylistAdd,
        Icon::PlaylistPlay,
        Icon::List,
        Icon::Radio,
        Icon::Search,
        Icon::Settings,
        Icon::MoreVert,
        Icon::MoreHoriz,
        Icon::DragHandle,
        Icon::ChevronDown,
        Icon::ChevronRight,
        Icon::ChevronLeft,
        Icon::ArrowBack,
        Icon::ArrowForward,
        Icon::ExpandMore,
        Icon::ExpandLess,
        Icon::Add,
        Icon::AddCircle,
        Icon::Delete,
        Icon::Close,
        Icon::Check,
        Icon::CheckCircle,
        Icon::Cancel,
        Icon::Edit,
        Icon::Share,
        Icon::Save,
        Icon::ContentCopy,
        Icon::Download,
        Icon::Upload,
        Icon::FileOpen,
        Icon::FolderOpen,
        Icon::OpenInNew,
        Icon::Refresh,
        Icon::Sync,
        Icon::Minimize,
        Icon::Maximize,
        Icon::Fullscreen,
        Icon::FullscreenExit,
        Icon::CloudOff,
        Icon::Offline,
        Icon::Info,
        Icon::Error,
        Icon::Warning,
        Icon::Timer,
        Icon::WifiOff,
        Icon::Mic,
        Icon::Headset,
        Icon::Lyrics,
        Icon::Speed,
        Icon::Equalizer,
        Icon::GraphicEq,
        Icon::AudioTrack,
        Icon::Image,
        Icon::Photo,
        Icon::FilterList,
        Icon::Sort,
        Icon::AccountCircle,
        Icon::Logout,
        Icon::Login,
        Icon::Notifications,
        Icon::Done,
        Icon::DoneAll,
        Icon::RadioButtonChecked,
        Icon::RadioButtonUnchecked,
        Icon::CheckBox,
        Icon::CheckBoxOutlineBlank,
    ];

    /// Codepoints of all icons (for [`Fonts::prewarm_icons`](crate::text::Fonts::prewarm_icons)).
    pub fn all_codepoints() -> Vec<char> {
        Self::ALL.iter().map(|i| i.codepoint()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codepoints_are_in_private_use_area() {
        for icon in Icon::ALL {
            let cp = icon.codepoint() as u32;
            assert!(
                (0xe000..=0xf8ff).contains(&cp) || (0xf0000..=0xffffd).contains(&cp),
                "{icon:?} codepoint U+{cp:04X} is not in a private use area"
            );
        }
    }

    #[test]
    fn all_is_complete_for_required_names() {
        // The task-mandated set must exist.
        let required = [
            Icon::Play,
            Icon::Pause,
            Icon::SkipNext,
            Icon::SkipPrev,
            Icon::Shuffle,
            Icon::Repeat,
            Icon::RepeatOne,
            Icon::Heart,
            Icon::HeartFilled,
            Icon::VolumeUp,
            Icon::VolumeOff,
            Icon::Search,
            Icon::Home,
            Icon::MusicNote,
            Icon::Settings,
            Icon::Download,
            Icon::PlaylistAdd,
            Icon::Add,
            Icon::MoreVert,
            Icon::Delete,
            Icon::Close,
            Icon::Minimize,
            Icon::Maximize,
            Icon::ChevronDown,
            Icon::Check,
            Icon::Upload,
            Icon::FileOpen,
            Icon::OpenInNew,
            Icon::Refresh,
            Icon::CloudOff,
            Icon::DragHandle,
            Icon::Queue,
            Icon::Like,
            Icon::Offline,
            Icon::Info,
        ];
        for icon in required {
            assert!(Icon::ALL.contains(&icon));
            assert!(icon.codepoint() as u32 >= 0xe000);
        }
    }

    #[test]
    fn all_codepoints_has_no_duplicates_within_font() {
        // Duplicates are allowed only when they are genuinely the same glyph
        // (e.g. CloudOff/Offline, ExpandMore/ChevronDown).
        use std::collections::HashMap;
        let mut seen: HashMap<char, Vec<Icon>> = HashMap::new();
        for icon in Icon::ALL {
            seen.entry(icon.codepoint()).or_default().push(*icon);
        }
        for (cp, icons) in seen {
            if icons.len() > 1 {
                assert!(
                    cp == Icon::CloudOff.codepoint()
                        || cp == Icon::ExpandMore.codepoint()
                        || cp == Icon::PlayCircle.codepoint(),
                    "unexpected duplicate codepoint U+{:04X} for {icons:?}",
                    cp as u32
                );
            }
        }
    }
}
