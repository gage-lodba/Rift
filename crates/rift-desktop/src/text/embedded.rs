//! Font bytes baked into the executable.
//!
//! Keeping the fonts embedded (rather than loading from disk at runtime) means
//! the binary is self-contained and text can never fail to render because a
//! font file went missing beside the executable. The files live in
//! `crates/rift-desktop/assets/fonts/` next to their licenses:
//!
//! * Inter (SIL Open Font License 1.1) — `Inter-LICENSE.txt`, from the official
//!   <https://github.com/rsms/inter> v4.1 release.
//! * Material Icons (Apache-2.0) — `MaterialIcons-LICENSE.txt`, from
//!   <https://github.com/google/material-design-icons>.
//!
//! Only the four static Inter weights the UI actually uses are embedded; the
//! variable font and italics are not needed for a chrome-only desktop player.

/// Inter Regular (400).
pub(crate) static INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
/// Inter Medium (500).
pub(crate) static INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
/// Inter `SemiBold` (600).
pub(crate) static INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
/// Inter Bold (700).
pub(crate) static INTER_BOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-Bold.ttf");
/// Material Icons Regular (filled). Apache-2.0.
pub(crate) static ICONS_FILLED: &[u8] =
    include_bytes!("../../assets/fonts/MaterialIcons-Regular.ttf");
/// Material Icons Outlined. Apache-2.0.
pub(crate) static ICONS_OUTLINED: &[u8] =
    include_bytes!("../../assets/fonts/MaterialIconsOutlined-Regular.otf");
