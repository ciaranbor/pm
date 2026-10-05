//! What init adds to the status line: the attention summary at the start
//! of `status-right`, and pm's announcements at the end of
//! `status-format[0]`.
//!
//! `status-right` is cut to `status-right-length` from its right end, so
//! the summary survives where the theme's own items may not.
//!
//! The announcement goes last, centred between the left side (status-left
//! and the window list) and `status-right`, so it never covers either and
//! nothing after it depends on alignment, whatever the format holds. It is
//! read from options rather than written into the format, because the
//! status line passes its text through strftime: a `%` in the text, or a
//! pane id, would be mangled. A window with `@pm_announcement_hidden` set
//! shows its `@pm_announcement_window` instead: the text without the ask of
//! the agent in that window.

pub(super) const SUMMARY: &str = "#{?@pm_summary,#{E:@pm_summary} ,}";
const SUMMARY_OPTION: &str = "@pm_summary";

pub(super) const ANNOUNCEMENT: &str = "#[nolist align=centre norange default]\
    #{?@pm_announcement_hidden,#{@pm_announcement_window},#{@pm_announcement}}";
const ANNOUNCEMENT_OPTION: &str = "@pm_announcement";

/// `status-right` as init wants it: with `on`, pm's summary first, unless
/// the user placed `@pm_summary` themselves; without, none of pm's.
pub(super) fn status_right(format: &str, on: bool) -> String {
    let base = format.strip_prefix(SUMMARY).unwrap_or(format);
    if !on || base.contains(SUMMARY_OPTION) {
        return base.to_string();
    }
    format!("{SUMMARY}{base}")
}

/// `status-format[0]` as init wants it: with `on`, pm's announcement last,
/// unless the user placed `@pm_announcement` themselves; without, none of
/// pm's.
pub(super) fn status_format(format: &str, on: bool) -> String {
    let base = format.strip_suffix(ANNOUNCEMENT).unwrap_or(format);
    if !on || base.contains(ANNOUNCEMENT_OPTION) {
        return base.to_string();
    }
    format!("{base}{ANNOUNCEMENT}")
}
