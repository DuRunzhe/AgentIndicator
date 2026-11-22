//! The accessibility rung of the DeepSeek Harness session ladder.
//!
//! The desktop shell answers only `dsh://open`, and its frontend keeps the open
//! conversation in memory with no URL of its own, so the only way to reach a
//! conversation from outside is to drive the sidebar the way a person does: find
//! the conversation's row in the application's accessibility tree and press it.
//!
//! That path needs two things the user owns — the accessibility permission, and
//! a window on screen — so every step reports failure instead of guessing, and
//! the caller's ladder falls back to the window. Concretely:
//!
//! * `press_session` refuses without the permission (`authorized`).
//! * Chromium builds its accessibility tree only once a client asks for it, so
//!   the tree is bootstrapped with `AXManualAccessibility` and then polled; a
//!   slow build is a wait, not a failure.
//! * The press is confirmed by reading the window title afterwards: an
//!   unverified press is reported as undelivered, which is what lets a future
//!   app version that moves its sidebar degrade to the window instead of doing
//!   nothing visible.

#![cfg(target_os = "macos")]

use crate::deepseek_jump::SessionTarget;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use objc2_foundation::NSString;
use std::{
    ffi::{c_void, CString},
    thread::sleep,
    time::{Duration, Instant},
};

/// `AXError.success`.
const AX_SUCCESS: i32 = 0;
/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// How long the application's accessibility tree may take to appear from cold.
const TREE_WAIT: Duration = Duration::from_secs(3);
/// How long the window title may take to follow a press. Opening a large
/// conversation reloads it before the title changes, which can take a moment.
const PRESS_WAIT: Duration = Duration::from_millis(3000);
/// Poll interval for both waits.
const POLL: Duration = Duration::from_millis(100);
/// Upper bound on the nodes one walk visits. The web area is a browser DOM tree;
/// a pathological one must not stall the tray's click handler.
const NODE_BUDGET: usize = 20_000;
/// Upper bound on tree depth.
const MAX_DEPTH: usize = 40;

type Ptr = *mut c_void;
type CfPtr = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CfPtr) -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> Ptr;
    fn AXUIElementCopyAttributeValue(element: Ptr, attribute: CfPtr, out: *mut CfPtr) -> i32;
    fn AXUIElementSetAttributeValue(element: Ptr, attribute: CfPtr, value: CfPtr) -> i32;
    fn AXUIElementCopyActionNames(element: Ptr, out: *mut CfPtr) -> i32;
    fn AXUIElementPerformAction(element: Ptr, action: CfPtr) -> i32;
    /// `AXTrustedCheckOptionPrompt`, a CFString key consumed by
    /// [`AXIsProcessTrustedWithOptions`].
    static kAXTrustedCheckOptionPrompt: CfPtr;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithBytes(
        allocator: CfPtr,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        is_external: bool,
    ) -> CfPtr;
    fn CFStringGetCString(string: CfPtr, buffer: *mut i8, size: isize, encoding: u32) -> bool;
    fn CFArrayGetCount(array: CfPtr) -> isize;
    fn CFArrayGetValueAtIndex(array: CfPtr, index: isize) -> Ptr;
    fn CFDictionaryCreate(
        allocator: CfPtr,
        keys: *const CfPtr,
        values: *const CfPtr,
        count: isize,
        key_callbacks: CfPtr,
        value_callbacks: CfPtr,
    ) -> CfPtr;
    fn CFGetTypeID(value: CfPtr) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFRetain(value: CfPtr) -> CfPtr;
    fn CFRelease(value: CfPtr);
    static kCFBooleanTrue: CfPtr;
}

/// One owned `AXUIElementRef`.
///
/// Every element held here is retained, so releasing the array an element was
/// read from cannot invalidate it mid-walk.
struct Element(Ptr);

impl Element {
    fn application(pid: i32) -> Option<Self> {
        let pointer = unsafe { AXUIElementCreateApplication(pid) };
        (!pointer.is_null()).then_some(Self(pointer))
    }

    fn attribute(&self, name: &str) -> Option<OwnedValue> {
        let attribute = cf_string(name)?;
        let mut value: CfPtr = std::ptr::null();
        let status =
            unsafe { AXUIElementCopyAttributeValue(self.0, attribute.pointer, &mut value) };
        let value = OwnedValue { pointer: value };
        if status != AX_SUCCESS || value.pointer.is_null() {
            return None;
        }
        Some(value)
    }

    fn set_flag(&self, name: &str) -> bool {
        let Some(attribute) = cf_string(name) else {
            return false;
        };
        unsafe {
            AXUIElementSetAttributeValue(self.0, attribute.pointer, kCFBooleanTrue) == AX_SUCCESS
        }
    }

    fn actions(&self) -> Vec<String> {
        let mut names: CfPtr = std::ptr::null();
        if unsafe { AXUIElementCopyActionNames(self.0, &mut names) } != AX_SUCCESS
            || names.is_null()
        {
            return Vec::new();
        }
        let names = OwnedValue { pointer: names };
        names.strings()
    }

    fn perform(&self, action: &str) -> bool {
        let Some(action) = cf_string(action) else {
            return false;
        };
        unsafe { AXUIElementPerformAction(self.0, action.pointer) == AX_SUCCESS }
    }

    /// This element's children, each retained for the duration of the walk.
    fn children(&self) -> Vec<Self> {
        let Some(children) = self.attribute("AXChildren") else {
            return Vec::new();
        };
        children
            .elements()
            .into_iter()
            .map(|child| {
                // SAFETY: `child` came out of a live AX array; retaining it makes the
                // handle independent of the array's own lifetime.
                unsafe { CFRetain(child) };
                Self(child)
            })
            .collect()
    }

    /// A single element attribute, such as `AXMainWindow`.
    fn element(&self, name: &str) -> Option<Self> {
        let value = self.attribute(name)?;
        if value.pointer.is_null() {
            return None;
        }
        // SAFETY: an element-valued attribute is an AXUIElementRef; retaining it
        // lets it outlive the attribute read.
        unsafe { CFRetain(value.pointer) };
        Some(Self(value.pointer.cast_mut()))
    }

    fn string(&self, name: &str) -> Option<String> {
        self.attribute(name)?.string()
    }
}

impl Drop for Element {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

/// A `+1` CoreFoundation value, released on drop.
struct OwnedValue {
    pointer: CfPtr,
}

impl OwnedValue {
    fn string(&self) -> Option<String> {
        if self.pointer.is_null() {
            return None;
        }
        // SAFETY: the type id is compared before the value is read as a string.
        if unsafe { CFGetTypeID(self.pointer) } != unsafe { CFStringGetTypeID() } {
            return None;
        }
        let mut buffer = vec![0i8; 4096];
        // SAFETY: the buffer is valid for its whole length and the encoding is
        // the one used to create the strings we read.
        let ok = unsafe {
            CFStringGetCString(
                self.pointer,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
                UTF8,
            )
        };
        if !ok {
            return None;
        }
        // SAFETY: on success CFStringGetCString NUL-terminates the buffer.
        let text = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
        Some(text.to_string_lossy().into_owned())
    }

    /// The retained elements of an `AXChildren` array.
    fn elements(&self) -> Vec<Ptr> {
        if self.pointer.is_null() {
            return Vec::new();
        }
        // SAFETY: only called on AX arrays, whose members are elements.
        let count = unsafe { CFArrayGetCount(self.pointer) };
        (0..count.max(0))
            .filter_map(|index| {
                // SAFETY: `index` is within the array's count.
                let element = unsafe { CFArrayGetValueAtIndex(self.pointer, index) };
                (!element.is_null()).then_some(element)
            })
            .collect()
    }

    fn strings(&self) -> Vec<String> {
        self.elements()
            .into_iter()
            .filter_map(|pointer| {
                let value = OwnedValue {
                    pointer: unsafe { CFRetain(pointer) },
                };
                let text = value.string();
                drop(value);
                text
            })
            .collect()
    }
}

impl Drop for OwnedValue {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            unsafe { CFRelease(self.pointer) };
        }
    }
}

/// An owned CoreFoundation string built from Rust text, released on drop.
fn cf_string(value: &str) -> Option<OwnedValue> {
    let bytes = CString::new(value).ok()?;
    // SAFETY: the bytes are a live NUL-terminated UTF-8 buffer for the call, and
    // the result is checked before it is wrapped.
    let string = unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            bytes.as_ptr().cast(),
            bytes.as_bytes().len() as isize,
            UTF8,
            false,
        )
    };
    (!string.is_null()).then_some(OwnedValue { pointer: string })
}

/// Whether macOS granted this application the accessibility permission.
pub fn authorized() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Ask for the permission: shows the system's own prompt, which also adds this
/// application to the Accessibility list so the user only has to switch it on.
pub fn request_authorization() -> bool {
    // The prompt key ships as a symbol; the dictionary borrows it and the
    // boolean, so no retain/release callbacks are needed.
    // SAFETY: both pointers are the framework's own static constants, and the
    // dictionary is a one-entry map of them.
    let options = unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            [kCFBooleanTrue].as_ptr(),
            1,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if options.is_null() {
        return false;
    }
    let options = OwnedValue { pointer: options };
    // SAFETY: the dictionary is live for the duration of the call.
    unsafe { AXIsProcessTrustedWithOptions(options.pointer) != 0 }
}

/// The running desktop application's pid, if it is open.
pub fn desktop_pid() -> Option<i32> {
    let identifier = NSString::from_str(crate::deepseek_jump::BUNDLE_ID);
    let applications = NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier);
    applications
        .iter()
        .next()
        .map(|application| application.processIdentifier())
}

/// One conversation row as the sidebar renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The row's own text, which is the conversation title followed by its
    /// relative age (`同步最新代码 2天`).
    pub label: String,
    /// The workspace group the row sits in, which is the project directory's
    /// name.
    pub workspace: Option<String>,
}

/// The index of the row that names `target`.
///
/// Titles are not unique — several workspaces can hold a conversation with the
/// same name — so a row that carries its workspace is matched inside that group
/// first, an exact title beats a prefix, and list order breaks the remaining
/// ties. A row is never matched by a title that only appears elsewhere in the
/// row, so an age cannot make a wrong conversation look right.
pub fn match_row(rows: &[Row], target: &SessionTarget) -> Option<usize> {
    let wanted = target.title.as_deref()?.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    let compact_wanted = compact(&wanted);
    let mut best: Option<(u8, usize)> = None;
    for (index, row) in rows.iter().enumerate() {
        let label = row.label.trim().to_lowercase();
        let compact = compact(&label);
        // The row's own text is the conversation title; a row that only exposes a
        // combined description adds the relative age, which is why a prefix of
        // the title followed by a space is accepted too. Spacing differences
        // between the projection cache and the rendered text are absorbed by the
        // compact comparison.
        let strength = if label == wanted {
            4
        } else if compact == compact_wanted {
            3
        } else if carries_age(&label, &wanted) || carries_age(&compact, &compact_wanted) {
            2
        } else if compact.starts_with(&compact_wanted) && compact_wanted.chars().count() >= 4 {
            // The sidebar can decorate a title this build does not know about
            // (a badge, a mention, a truncated suffix); a long shared prefix is
            // still that conversation. The press is verified by the window title,
            // so a wrong guess degrades to the window instead of misleading.
            1
        } else {
            continue;
        };
        let workspace = match (&target.workspace, &row.workspace) {
            (None, _) => true,
            (Some(wanted), Some(actual)) => wanted.eq_ignore_ascii_case(actual),
            (Some(_), None) => false,
        };
        // Score: the workspace the row belongs to, then how closely the title
        // matches, then list order.
        let score = u8::from(workspace) * 4 + strength;
        if best.is_none_or(|(best_score, _)| score > best_score) {
            best = Some((score, index));
        }
    }
    best.map(|(_, index)| index)
}

/// The only row in `target`'s workspace whose text is a working/waiting status.
///
/// The sidebar replaces the title of an active or running conversation with a
/// status word ("进行中", "等待回答"), so a working session has no title text to
/// match. When the group holds exactly one such row, pressing it is unambiguous;
/// two or more status rows in one workspace are left to the window fallback
/// rather than guessing which running conversation the user meant.
fn only_status_row(rows: &[Row], target: &SessionTarget) -> Option<usize> {
    let workspace = target.workspace.as_deref()?;
    let mut candidates = rows.iter().enumerate().filter(|(_, row)| {
        row.workspace
            .as_deref()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(workspace))
            && is_status_label(&row.label)
    });
    let (index, _) = candidates.next()?;
    candidates.next().is_none().then_some(index)
}

/// Whether a sidebar row shows a conversation status instead of its title.
fn is_status_label(label: &str) -> bool {
    matches!(
        label.trim().to_lowercase().as_str(),
        "进行中"
            | "运行中"
            | "生成中"
            | "等待回答"
            | "等待回复"
            | "等待确认"
            | "等待操作"
            | "working"
            | "running"
            | "waiting for reply"
            | "waiting for confirmation"
    )
}

/// A comparable form of a title: lowercase, and without the whitespace or
/// punctuation a renderer is free to add (Chinese titles frequently carry no
/// spaces while the cache does).
fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric() || !ch.is_ascii())
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// Whether `label` is `title` followed by its relative age, which is what a row
/// that exposes only a combined description looks like.
///
/// An age always starts with a digit (`2天`, `3 days`, `11h`), so the rule is
/// narrow on purpose: a title that merely continues the target (`同步` inside
/// `同步最新代码`) must not match, or a click could press another conversation.
/// The press is confirmed by the window title anyway, so even a wrong guess
/// degrades to the window rather than leaving the user somewhere unexpected.
fn carries_age(label: &str, title: &str) -> bool {
    label
        .strip_prefix(title)
        .map(str::trim_start)
        .is_some_and(|rest| {
            rest.len() < label.len()
                && !rest.is_empty()
                && rest.starts_with(|ch: char| ch.is_ascii_digit())
        })
}

/// Bring the conversation `target` up in the desktop window.
///
/// Returns only after the window title confirms the switch, so a press that the
/// app ignored is reported as undelivered rather than as success.
pub fn press_session(target: &SessionTarget) -> bool {
    if !authorized() {
        // The permission is per code signature, so a fresh build (or one installed
        // at another path) has to be granted again; say so rather than looking
        // like a broken feature.
        crate::deepseek_jump::log("press stage=trust authorized=false");
        return false;
    }
    let Some(pid) = desktop_pid() else {
        crate::deepseek_jump::log("press stage=pid found=false");
        return false;
    };
    // The window may be minimized or closed; `dsh://open` is the app's own way to
    // restore or recreate it, and it is also the ladder's last rung.
    let _ = crate::deepseek_jump::open_window();
    let identifier = NSString::from_str(crate::deepseek_jump::BUNDLE_ID);
    if let Some(application) =
        NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier)
            .iter()
            .next()
    {
        application.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }
    // The app may already be showing the requested conversation: its sidebar
    // then replaces the row title with a status word ("进行中", "等待回答"), so
    // the row scan cannot match a title that is no longer rendered. The window
    // title is the authoritative answer in that case, and a click that is
    // already there is a successful jump rather than a failed press.
    if window_matches(pid, target) {
        let window = window_title(pid).unwrap_or_default();
        crate::deepseek_jump::log(&format!(
            "press stage=current matched=true pid={pid} window={window:?} title={:?}",
            target.title
        ));
        return true;
    }
    let Some(root) = Element::application(pid) else {
        crate::deepseek_jump::log("press stage=element found=false");
        return false;
    };
    // Chromium builds the renderer's accessibility tree only once a client asks
    // for it; this attribute is how a client asks.
    root.set_flag("AXManualAccessibility");

    let deadline = Instant::now() + TREE_WAIT;
    loop {
        let mut rows: Vec<(Row, Element)> = Vec::new();
        collect_rows(&root, None, 0, &mut rows, &mut 0);
        let labels: Vec<Row> = rows.iter().map(|(row, _)| row.clone()).collect();
        let matched = match_row(&labels, target).or_else(|| only_status_row(&labels, target));
        let detail = format!(
            "windows={} rows={} matched={} workspace={:?}",
            window_count(&root),
            rows.len(),
            matched.map_or("none".to_owned(), |index| index.to_string()),
            target.workspace
        );
        if let Some(index) = matched {
            let (row, element) = &rows[index];
            let actions = element.actions();
            if !actions.iter().any(|action| action == "AXPress") {
                crate::deepseek_jump::log(&format!(
                    "press stage=press actions={actions:?} row={:?} {detail}",
                    row.label
                ));
                return false;
            }
            element.perform("AXScrollToVisible");
            let pressed = element.perform("AXPress");
            let confirmed = pressed && confirmed(pid, target);
            crate::deepseek_jump::log(&format!(
                "press stage=confirm pressed={pressed} confirmed={confirmed} row={:?} {detail}",
                row.label
            ));
            return confirmed;
        }
        if Instant::now() >= deadline {
            // The labels are the only way to tell "the conversation is gone" from
            // "the sidebar names it differently", and the two need opposite fixes.
            let seen: Vec<String> = labels
                .iter()
                .take(24)
                .map(|row| format!("{}|{}", row.workspace.as_deref().unwrap_or("-"), row.label))
                .collect();
            let window = Element::application(pid)
                .and_then(|root| root.element("AXMainWindow"))
                .and_then(|window| window.string("AXTitle"))
                .unwrap_or_default();
            crate::deepseek_jump::log(&format!(
                "press stage=tree timeout={} {detail} pid={pid} window={window:?} title={:?} rows={seen:?}",
                TREE_WAIT.as_millis(),
                target.title
            ));
            return false;
        }
        sleep(POLL);
    }
}

/// How many windows the application currently reports.
fn window_count(root: &Element) -> usize {
    root.attribute("AXWindows")
        .map(|windows| windows.elements().len())
        .unwrap_or(0)
}

/// The title of the desktop application's main window.
fn window_title(pid: i32) -> Option<String> {
    Element::application(pid)?
        .element("AXMainWindow")?
        .string("AXTitle")
}

/// Whether the current window title names `target`.
///
/// The projection cache can truncate the title and the window can decorate it
/// with a suffix, so the cache's title is accepted as a prefix of the real one.
fn window_matches(pid: i32, target: &SessionTarget) -> bool {
    let Some(title) = target.title.as_deref() else {
        return false;
    };
    window_title(pid).is_some_and(|current| title_matches(&current, title))
}

/// Whether `current` is the window title for `target`. Kept pure for the test:
/// the AX call needs the desktop app, the comparison does not.
fn title_matches(current: &str, target: &str) -> bool {
    let target = target.trim();
    // Four characters is the same floor `match_row` uses; a shorter prefix is
    // not enough to claim the window is showing this conversation.
    target.chars().count() >= 4 && current.starts_with(target)
}

/// Whether the window now shows `target`.
fn confirmed(pid: i32, target: &SessionTarget) -> bool {
    if target.title.as_deref().is_none() {
        // Without a title there is nothing to confirm against, so the press is
        // reported as undelivered and the ladder falls back to the window.
        return false;
    }
    let deadline = Instant::now() + PRESS_WAIT;
    loop {
        if window_matches(pid, target) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(POLL);
    }
}

/// Collect the sidebar rows, in list order, remembering their workspace group.
///
/// The tree is walked once: each row keeps its own element so the match a caller
/// makes on the labels can be pressed without a second walk that could disagree
/// with the first if the window changed in between.
fn collect_rows(
    element: &Element,
    workspace: Option<&str>,
    depth: usize,
    rows: &mut Vec<(Row, Element)>,
    visited: &mut usize,
) {
    if depth > MAX_DEPTH || *visited > NODE_BUDGET {
        return;
    }
    *visited += 1;
    let role = element.string("AXRole").unwrap_or_default();
    let children = element.children();
    if role == "AXRow" {
        if let Some(label) = row_label(element) {
            rows.push((
                Row {
                    label,
                    workspace: workspace.map(str::to_owned),
                },
                Element(unsafe { CFRetain(element.0) }.cast_mut()),
            ));
        }
        return;
    }
    // A workspace is an outline group whose first row carries the project name;
    // every later row in that group is one of its conversations.
    let next_workspace = if role == "AXOutline" || role == "AXGroup" {
        group_name(element).or_else(|| workspace.map(str::to_owned))
    } else {
        workspace.map(str::to_owned)
    };
    for child in children {
        collect_rows(&child, next_workspace.as_deref(), depth + 1, rows, visited);
    }
}

/// A group's own name, read from its first row (the workspace header).
fn group_name(element: &Element) -> Option<String> {
    let children = element.children();
    let first = children.first()?;
    if first.string("AXRole").as_deref() != Some("AXRow") {
        return None;
    }
    row_label(first)
}

/// A row's text: the conversation title the sidebar renders for it.
///
/// A row may carry more than one text — the title, then its relative age — and a
/// row that exposes only a combined description adds the age after a space. The
/// age is never the title, so it is skipped rather than matched against: a jump
/// that compared `4分钟` with a conversation name would always fail.
fn row_label(element: &Element) -> Option<String> {
    let mut texts: Vec<String> = Vec::new();
    for child in element.children() {
        if child.string("AXRole").as_deref() != Some("AXStaticText") {
            continue;
        }
        if let Some(value) = child.string("AXValue").or_else(|| child.string("AXTitle")) {
            let value = value.trim().to_owned();
            if !value.is_empty() {
                texts.push(value);
            }
        }
    }
    texts
        .iter()
        .find(|value| !is_relative_age(value))
        .cloned()
        .or_else(|| texts.first().cloned())
        .or_else(|| {
            element
                .string("AXDescription")
                .filter(|value| !value.trim().is_empty())
        })
}

/// Whether a text is only the relative age the sidebar shows (`4分钟`, `2天`,
/// `11d`, `3 days`), which never names a conversation.
///
/// The rule is deliberately strict: a number, then one unit word, then the end of
/// the text. A title may start with a number too (`3D 渲染`, `2.1.70用户标签…`),
/// and skipping it would lose the row it names.
fn is_relative_age(value: &str) -> bool {
    const UNITS: [&str; 26] = [
        "秒", "分钟", "分", "小时", "时", "天", "日", "周", "个月", "月", "年", "s", "m", "h", "d",
        "w", "y", "sec", "secs", "min", "mins", "hr", "hrs", "hour", "hours", "day",
    ];
    const LONG_UNITS: [&str; 6] = ["days", "weeks", "week", "months", "month", "years"];
    let value = value.trim();
    let digits = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == ' ')
        .count();
    if digits == 0 {
        return false;
    }
    let rest = value[digits..].trim().to_lowercase();
    if rest.is_empty() {
        return false;
    }
    let token: String = rest.chars().take_while(|ch| ch.is_alphabetic()).collect();
    // Anything after the unit means the text says more than how old it is.
    if token.is_empty() || token != rest {
        return false;
    }
    UNITS.contains(&token.as_str()) || LONG_UNITS.contains(&token.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(title: &str, workspace: Option<&str>) -> SessionTarget {
        SessionTarget {
            id: "session-1".into(),
            title: Some(title.into()),
            workspace: workspace.map(str::to_owned),
        }
    }

    fn row(label: &str, workspace: Option<&str>) -> Row {
        Row {
            label: label.into(),
            workspace: workspace.map(str::to_owned),
        }
    }

    #[test]
    fn a_row_is_matched_by_its_own_title_and_age() {
        let rows = vec![
            row("搭建局域网Git协议仓库", Some("nita")),
            row("同步最新代码 2天", Some("adb-wifi-manager")),
            row("同步最新代码 9天", Some("nita")),
        ];
        // The label carries the relative age, so a prefix match is the normal
        // case, not an exact one.
        assert_eq!(
            match_row(&rows, &target("搭建局域网Git协议仓库", None)),
            Some(0)
        );
        // A row that only exposes a combined description carries the relative
        // age after the title.
        assert_eq!(
            match_row(
                &[row("搭建局域网Git协议仓库 2天", Some("nita"))],
                &target("搭建局域网Git协议仓库", None)
            ),
            Some(0)
        );
        assert_eq!(match_row(&rows, &target("同步最新代码", None)), Some(1));
    }

    #[test]
    fn a_workspace_picks_between_same_titled_conversations() {
        let rows = vec![
            row("同步最新代码 2天", Some("adb-wifi-manager")),
            row("同步最新代码 9天", Some("nita")),
        ];
        assert_eq!(
            match_row(&rows, &target("同步最新代码", Some("nita"))),
            Some(1),
            "the workspace the row belongs to must win over list order"
        );
        assert_eq!(
            match_row(&rows, &target("同步最新代码", Some("adb-wifi-manager"))),
            Some(0)
        );
        // An unknown workspace still matches one of them rather than nothing.
        assert_eq!(
            match_row(&rows, &target("同步最新代码", Some("elsewhere"))),
            Some(0)
        );
    }

    #[test]
    fn a_truncated_target_title_matches_the_window_title() {
        // The tray's projection cache truncates long titles; the window carries
        // the full one. A target that is already open must count as delivered.
        assert!(title_matches(
            "Implement authorization guide per onboarding doc — DeepSeek Harness",
            "Implement authorization ",
        ));
        assert!(!title_matches(
            "查看版本与tag信息 — DeepSeek Harness",
            "Implement authorization",
        ));
        assert!(!title_matches("Fix the parser — DeepSeek Harness", "Fix"));
    }

    #[test]
    fn a_single_status_row_identifies_a_running_conversation() {
        let rows = vec![
            row("AgentIndicator", Some("AgentIndicator")),
            row("进行中", Some("AgentIndicator")),
            row("同步最新代码", Some("AgentIndicator")),
            row("进行中", Some("touch-mouse")),
        ];
        assert_eq!(
            only_status_row(
                &rows,
                &target("支持 Gemini、Grok、Cursor CLI", Some("AgentIndicator"))
            ),
            Some(1)
        );
        // Two running conversations in one group are ambiguous.
        let rows = vec![
            row("进行中", Some("AgentIndicator")),
            row("进行中", Some("AgentIndicator")),
        ];
        assert_eq!(
            only_status_row(&rows, &target("any", Some("AgentIndicator"))),
            None
        );
        // Without a workspace there is nothing to scope the status to.
        assert_eq!(only_status_row(&rows, &target("any", None)), None);
    }

    #[test]
    fn an_age_or_a_longer_title_cannot_make_a_wrong_row_look_right() {
        let rows = vec![
            row("同步最新代码 2天", Some("nita")),
            row("查看当前工程 3天", Some("skills")),
        ];
        assert_eq!(match_row(&rows, &target("同步", None)), None);
        assert_eq!(match_row(&rows, &target("工程", None)), None);
        // A trailing word of the title is not a prefix of the label.
        assert_eq!(match_row(&rows, &target("最新代码", None)), None);
        // Empty and missing titles cannot address anything.
        assert_eq!(match_row(&rows, &target("", None)), None);
        assert_eq!(
            match_row(
                &rows,
                &SessionTarget {
                    id: "s".into(),
                    title: None,
                    workspace: None
                }
            ),
            None
        );
    }

    #[test]
    #[ignore = "drives the running desktop application"]
    fn smoke_reproduce_from_url() {
        // Exactly what a tray click carries, decoded the same way.
        let url = "dsh://session/session-3c4b74ef-c82e-4812-9602-0caedc4a8b87?title=%E8%B0%83%E8%AF%95%20APP%20ANR%20%E5%8E%9F%E5%9B%A0&workspace=manis-mubao";
        let target = crate::deepseek_jump::SessionTarget::parse(url).expect("target");
        println!("parsed: {:?}", target);
        // Hide the window first, the way opening the tray menu does.
        let _ = std::process::Command::new("/usr/bin/open")
            .args(["-a", "Finder"])
            .status();
        sleep(Duration::from_millis(800));
        let outcome = crate::deepseek_jump::open(url, crate::deepseek_jump::Options::default());
        println!("outcome: {outcome:?}");
    }

    #[test]
    #[ignore = "drives the running desktop application"]
    fn smoke_dump_rows() {
        let Some(pid) = desktop_pid() else {
            panic!("desktop app is not running");
        };
        let root = Element::application(pid).expect("application element");
        root.set_flag("AXManualAccessibility");
        sleep(Duration::from_millis(800));
        let mut rows: Vec<(Row, Element)> = Vec::new();
        collect_rows(&root, None, 0, &mut rows, &mut 0);
        println!("rows: {}", rows.len());
        for (row, _) in &rows {
            println!("ws={:?} label={:?}", row.workspace, row.label);
        }
        let target = SessionTarget {
            id: "session-3c4b74ef-c82e-4812-9602-0caedc4a8b87".into(),
            title: Some("调试 APP ANR 原因".into()),
            workspace: Some("manis-mubao".into()),
        };
        let labels: Vec<Row> = rows.iter().map(|(row, _)| row.clone()).collect();
        println!("matched: {:?}", match_row(&labels, &target));
    }

    #[test]
    #[ignore = "drives the running desktop application"]
    fn smoke_press_session() {
        let target = SessionTarget {
            id: "smoke".into(),
            title: Some("查看当前工程".into()),
            workspace: Some("skills".into()),
        };
        assert!(authorized(), "the accessibility permission is required");
        assert!(press_session(&target), "the sidebar row must be pressed");
    }

    #[test]
    fn an_age_is_never_read_as_a_title() {
        // The sidebar prints the relative age next to the title, and a row that
        // exposes only a combined description joins them. A jump must never
        // compare an age with a conversation name.
        for age in ["4分钟", "2天", "11天", "24天", "1h", "3 days", "2 周"] {
            assert!(is_relative_age(age), "{age}");
        }
        for title in [
            "2.1.70用户标签UI调整排序",
            "2024 计划",
            "同步最新代码",
            "3D 渲染",
            "调试 APP ANR 原因",
        ] {
            assert!(!is_relative_age(title), "{title}");
        }
    }

    #[test]
    fn a_decorated_row_still_names_its_conversation() {
        // A sidebar free to add a badge, a mention or a truncation marker must not
        // hide the conversation from the jump.
        let rows = vec![row("调试 APP ANR 原因 有新回复", Some("manis-mubao"))];
        assert_eq!(
            match_row(&rows, &target("调试 APP ANR 原因", Some("manis-mubao"))),
            Some(0)
        );
        // A prefix shorter than four characters is not evidence of anything.
        let rows = vec![row("调试", Some("manis-mubao"))];
        assert_eq!(
            match_row(&rows, &target("调试 APP ANR 原因", Some("manis-mubao"))),
            None
        );
    }

    #[test]
    fn matching_ignores_case_and_surrounding_space() {
        let rows = vec![row("Fix The Build 1h", Some("Repo"))];
        assert_eq!(match_row(&rows, &target("  fix the build ", None)), Some(0));
        assert_eq!(match_row(&rows, &target("FIX THE BUILD", None)), Some(0));
        // The projection cache and the rendered sidebar can differ in spacing
        // without naming different conversations.
        assert_eq!(
            match_row(
                &[row("FixTheBuild 1h", Some("Repo"))],
                &target("Fix The Build", None)
            ),
            Some(0)
        );
    }
}
