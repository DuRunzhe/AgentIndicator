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
//! * A conversation row prints the conversation's state (`进行中`, `等待审批`,
//!   `已完成`) ahead of its title, as visually hidden text the tree exposes as
//!   its own static text: the title is picked out of the row's texts rather than
//!   assumed to be the first one.
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

    /// A string-array attribute, such as `AXDOMClassList`.
    fn strings(&self, name: &str) -> Vec<String> {
        self.attribute(name)
            .map(|value| value.strings())
            .unwrap_or_default()
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
    /// The title the row renders for its conversation.
    ///
    /// The sidebar prints a conversation's state in front of its title, inside
    /// the same row, as visually hidden text: `进行中`, `等待审批`,
    /// `1 个子智能体运行中`. The accessibility tree exposes both as static
    /// texts of the row, so the title has to be picked out of the row's texts
    /// instead of being assumed to be the first one.
    pub title: Option<String>,
    /// The state words the row prints in front of its title, in that order.
    pub status: Vec<String>,
    /// The workspace group the row sits in, which is the project directory's
    /// name.
    pub workspace: Option<String>,
}

/// The index of the row that names `target`.
///
/// Titles are not unique — several workspaces can hold a conversation with the
/// same name — so a row that carries its workspace is matched inside that group
/// first, an exact title beats a prefix, and list order breaks the remaining
/// ties. Only the row's title is compared, never a state word or an age that the
/// row also prints, so an age cannot make a wrong conversation look right.
pub fn match_row(rows: &[Row], target: &SessionTarget) -> Option<usize> {
    let wanted = target.title.as_deref()?.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    let compact_wanted = compact(&wanted);
    let mut best: Option<(u8, usize)> = None;
    for (index, row) in rows.iter().enumerate() {
        let Some(title) = row.title.as_deref() else {
            continue;
        };
        let label = title.trim().to_lowercase();
        let compact = compact(&label);
        // A row that only exposes a combined description carries the relative age
        // after the title, which is why a prefix of the title followed by a space
        // is accepted too. Spacing differences between the projection cache and
        // the rendered text are absorbed by the compact comparison.
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

/// The only row in `target`'s workspace that renders no title at all.
///
/// A row whose title is not rendered — a conversation the sidebar has not named,
/// or one whose title text this build keeps out of the accessibility tree — shows
/// nothing but its state. When the group holds exactly one such row, pressing it
/// is unambiguous; two or more are left to the window fallback rather than
/// guessing which conversation the user meant.
fn only_status_row(rows: &[Row], target: &SessionTarget) -> Option<usize> {
    let workspace = target.workspace.as_deref()?;
    let mut candidates = rows.iter().enumerate().filter(|(_, row)| {
        row.workspace
            .as_deref()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(workspace))
            && row.title.is_none()
            && !row.status.is_empty()
    });
    let (index, _) = candidates.next()?;
    candidates.next().is_none().then_some(index)
}

/// Whether a text is a conversation state rather than the conversation's name.
///
/// The vocabulary is the workspace sidebar's own: the state a conversation is in
/// (`进行中`, `等待审批`, `计划待审`, `等待回答`), its compact trailing form
/// (`待审批`, `待回答`), whether it finished (`已完成`), and whether it is idle
/// (`空闲`). A row prints these ahead of the title, which is what keeps
/// `row_parts` from reading one as a name.
fn is_status_label(label: &str) -> bool {
    let label = label.trim().to_lowercase();
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    matches!(
        label.as_str(),
        "进行中"
            | "运行中"
            | "生成中"
            | "空闲"
            | "等待回答"
            | "待回答"
            | "等待回复"
            | "等待确认"
            | "等待操作"
            | "等待审批"
            | "待审批"
            | "计划待审"
            | "已完成"
            | "running"
            | "working"
            | "idle"
            | "waiting"
            | "waiting for reply"
            | "waiting for answer"
            | "waiting for confirmation"
            | "waiting for approval"
            | "plan awaiting review"
            | "plan review"
            | "approval"
            | "answer"
            | "completed"
            | "complete"
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
    // The app may already be showing the requested conversation, and after
    // restoring a closed window it lands on the conversation it had open before,
    // which can be the one the click names. The window title is the authoritative
    // answer to "is it showing this conversation", so it is read on every poll
    // rather than once: a restore or a large conversation load takes a moment,
    // and giving up while the window is still catching up is exactly what makes a
    // jump degrade to "the app opened".
    let Some(root) = Element::application(pid) else {
        crate::deepseek_jump::log("press stage=element found=false");
        return false;
    };
    // Chromium builds the renderer's accessibility tree only once a client asks
    // for it; this attribute is how a client asks.
    root.set_flag("AXManualAccessibility");

    let deadline = Instant::now() + TREE_WAIT;
    loop {
        if window_matches(pid, target) {
            let window = window_title(pid).unwrap_or_default();
            crate::deepseek_jump::log(&format!(
                "press stage=current matched=true pid={pid} window={window:?} title={:?}",
                target.title
            ));
            return true;
        }
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
                    row_name(row)
                ));
                return false;
            }
            element.perform("AXScrollToVisible");
            let pressed = element.perform("AXPress");
            let confirmed = pressed && confirmed(pid, target);
            crate::deepseek_jump::log(&format!(
                "press stage=confirm pressed={pressed} confirmed={confirmed} row={:?} {detail}",
                row_name(row)
            ));
            return confirmed;
        }
        if Instant::now() >= deadline {
            // The titles are the only way to tell "the conversation is gone" from
            // "the sidebar names it differently", and the two need opposite fixes.
            let seen: Vec<String> = labels
                .iter()
                .take(24)
                .map(|row| {
                    format!(
                        "{}|{}",
                        row.workspace.as_deref().unwrap_or("-"),
                        row_name(row)
                    )
                })
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

/// A row as it reads in the log: its title, or the state it shows without one.
fn row_name(row: &Row) -> String {
    row.title
        .clone()
        .or_else(|| (!row.status.is_empty()).then(|| row.status.join(",")))
        .unwrap_or_default()
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
/// makes on the titles can be pressed without a second walk that could disagree
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
    if role == "AXRow" {
        // Only the sidebar's own conversation rows can be pressed. Workspace
        // headers and the rows the conversation transcript is built from are the
        // same role, and a jump must not press one of those by title.
        let classes = row_classes(element);
        if !classes.is_empty() && !is_session_row(&classes) {
            return;
        }
        let (title, status) = row_of(element);
        if title.is_none() && status.is_empty() {
            return;
        }
        rows.push((
            Row {
                title,
                status,
                workspace: workspace.map(str::to_owned),
            },
            Element(unsafe { CFRetain(element.0) }.cast_mut()),
        ));
        return;
    }
    // A workspace is an outline group whose first row carries the project name;
    // every later row in that group is one of its conversations.
    let mut children = element.children();
    let mut next_workspace = workspace.map(str::to_owned);
    if role == "AXOutline" || role == "AXGroup" {
        if let Some(first) = children.first() {
            let classes = row_classes(first);
            let header = first.string("AXRole").as_deref() == Some("AXRow")
                && (is_project_row(&classes) || classes.is_empty());
            if header {
                if let (Some(name), _) = row_of(first) {
                    next_workspace = Some(name);
                }
                children.remove(0);
            }
        }
    }
    for child in children {
        collect_rows(&child, next_workspace.as_deref(), depth + 1, rows, visited);
    }
}

/// The DOM classes the shell publishes for an element.
///
/// The sidebar's rows are the same `AXRow` role whether they are a workspace
/// header or a conversation; the class list is what tells them apart, which keeps
/// a workspace name from being matched as a conversation title.
fn row_classes(element: &Element) -> Vec<String> {
    element.strings("AXDOMClassList")
}

/// Whether a row is its workspace's header rather than a conversation.
fn is_project_row(classes: &[String]) -> bool {
    classes
        .iter()
        .any(|class| class.to_lowercase().contains("projectrow"))
}

/// Whether a row is one of the sidebar's conversations.
fn is_session_row(classes: &[String]) -> bool {
    classes
        .iter()
        .any(|class| class.to_lowercase().contains("sessionrow"))
}

/// The texts a row renders, in the order the sidebar prints them.
///
/// A conversation row prints its state words, then its title, then its relative
/// age, each as its own static text — so the title is never simply the first text
/// the row exposes.
fn row_texts(element: &Element) -> Vec<String> {
    element
        .children()
        .into_iter()
        .filter(|child| child.string("AXRole").as_deref() == Some("AXStaticText"))
        .filter_map(|child| child.string("AXValue").or_else(|| child.string("AXTitle")))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

/// A row's title and the states ahead of it.
///
/// A build that exposes a row's texts only as one combined description still
/// names its conversation: the description becomes the title, and the age it
/// carries is told apart by the comparison in [`match_row`].
fn row_of(element: &Element) -> (Option<String>, Vec<String>) {
    let texts = row_texts(element);
    if texts.is_empty() {
        return (
            element
                .string("AXDescription")
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            Vec::new(),
        );
    }
    row_parts(texts)
}

/// Split a row's texts into its conversation title and the states ahead of it.
///
/// The relative age is the last text and never names a conversation, so it is
/// dropped first. What remains is the row's state words followed by the title —
/// which makes the last text that is not a state the title, and every text before
/// it a state. A row that renders no title at all (a conversation the sidebar has
/// not named) keeps its states and reports no title, so it can still be pressed
/// through [`only_status_row`].
fn row_parts(mut texts: Vec<String>) -> (Option<String>, Vec<String>) {
    if texts.len() > 1 && texts.last().is_some_and(|last| is_relative_age(last)) {
        texts.pop();
    }
    match texts.iter().rposition(|text| !is_status_label(text)) {
        Some(index) => {
            let title = texts.remove(index);
            (Some(title), texts)
        }
        None => (None, texts),
    }
}

/// Whether a text is only the relative age the sidebar shows (`4分钟`, `2天`,
/// `11d`, `3 days`, `刚刚`), which never names a conversation.
///
/// The rule is deliberately strict: a number, then one unit word, then the end of
/// the text. A title may start with a number too (`3D 渲染`, `2.1.70用户标签…`),
/// and skipping it would lose the row it names.
fn is_relative_age(value: &str) -> bool {
    const UNITS: [&str; 27] = [
        "秒", "分钟", "分", "小时", "时", "天", "日", "周", "个月", "月", "年", "s", "m", "h", "d",
        "w", "y", "mo", "sec", "secs", "min", "mins", "hr", "hrs", "hour", "hours", "day",
    ];
    const LONG_UNITS: [&str; 7] = ["days", "week", "weeks", "month", "months", "year", "years"];
    // The recent past is not a number: the sidebar says `刚刚` / `now` / `jetzt`.
    const NOW: [&str; 8] = [
        "刚刚",
        "now",
        "今",
        "ahora",
        "maintenant",
        "jetzt",
        "сейчас",
        "agora",
    ];
    let value = value.trim();
    if NOW.iter().any(|now| value.eq_ignore_ascii_case(now)) {
        return true;
    }
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

    fn row(title: &str, workspace: Option<&str>) -> Row {
        Row {
            title: Some(title.into()),
            status: Vec::new(),
            workspace: workspace.map(str::to_owned),
        }
    }

    /// A row the sidebar prints a state or two in front of, the way it does for
    /// every conversation that is running, waiting or finished.
    fn running_row(title: &str, status: &[&str], workspace: Option<&str>) -> Row {
        Row {
            title: Some(title.into()),
            status: status.iter().map(|value| (*value).to_owned()).collect(),
            workspace: workspace.map(str::to_owned),
        }
    }

    /// A row that renders no title at all, only its state.
    fn status_row(status: &str, workspace: Option<&str>) -> Row {
        Row {
            title: None,
            status: vec![status.into()],
            workspace: workspace.map(str::to_owned),
        }
    }

    #[test]
    fn a_running_row_is_matched_by_the_title_behind_its_state() {
        // What the sidebar publishes for a conversation that is running or
        // waiting: the state first, then the title. A jump that only reads the
        // first text of the row reads `进行中` and never finds the conversation.
        let rows = vec![
            running_row(
                "支持 Gemini、Grok、Cursor CLI",
                &["进行中"],
                Some("AgentIndicator"),
            ),
            running_row("同步最新代码", &["等待审批"], Some("AgentIndicator")),
            row("手机模拟触控板控制电脑方案", Some("touch-mouse")),
        ];
        // The projection cache truncated the title; the row carries the rest.
        assert_eq!(
            match_row(
                &rows,
                &target("支持 Gemini、Grok、Cursor CL", Some("AgentIndicator"))
            ),
            Some(0)
        );
        assert_eq!(
            match_row(&rows, &target("同步最新代码", Some("AgentIndicator"))),
            Some(1)
        );
    }

    #[test]
    fn two_running_rows_in_one_workspace_are_told_apart_by_their_titles() {
        // Two conversations of the same workspace are running at once, so both
        // rows show the same state. Their titles are what tells them apart.
        let rows = vec![
            running_row("查看版本与tag信息", &["进行中"], Some("yimaplan")),
            running_row("App Store SDK版本警告处理", &["进行中"], Some("yimaplan")),
        ];
        assert_eq!(
            match_row(&rows, &target("查看版本与tag信息", Some("yimaplan"))),
            Some(0)
        );
        assert_eq!(
            match_row(
                &rows,
                &target("App Store SDK版本警告处理", Some("yimaplan"))
            ),
            Some(1)
        );
    }

    #[test]
    fn a_row_is_matched_by_its_own_title() {
        let rows = vec![
            row("搭建局域网Git协议仓库", Some("nita")),
            row("同步最新代码", Some("adb-wifi-manager")),
            row("同步最新代码", Some("nita")),
        ];
        assert_eq!(
            match_row(&rows, &target("搭建局域网Git协议仓库", None)),
            Some(0)
        );
        assert_eq!(match_row(&rows, &target("同步最新代码", None)), Some(1));
        // A row that exposes its text only as one combined description carries the
        // relative age after the title, and that is still the same conversation.
        assert_eq!(
            match_row(
                &[row("搭建局域网Git协议仓库 2天", Some("nita"))],
                &target("搭建局域网Git协议仓库", None)
            ),
            Some(0)
        );
    }

    #[test]
    fn a_workspace_picks_between_same_titled_conversations() {
        let rows = vec![
            row("同步最新代码", Some("adb-wifi-manager")),
            row("同步最新代码", Some("nita")),
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
    fn a_row_without_a_title_is_still_reachable_by_its_state() {
        let rows = vec![
            row("同步最新代码", Some("AgentIndicator")),
            status_row("进行中", Some("AgentIndicator")),
            status_row("等待审批", Some("touch-mouse")),
        ];
        assert_eq!(
            only_status_row(
                &rows,
                &target("支持 Gemini、Grok、Cursor CLI", Some("AgentIndicator"))
            ),
            Some(1)
        );
        // Two titleless rows in one workspace are ambiguous.
        let rows = vec![
            status_row("进行中", Some("AgentIndicator")),
            status_row("等待审批", Some("AgentIndicator")),
        ];
        assert_eq!(
            only_status_row(&rows, &target("any", Some("AgentIndicator"))),
            None
        );
        // Without a workspace there is nothing to scope the state to.
        assert_eq!(only_status_row(&rows, &target("any", None)), None);
    }

    #[test]
    fn a_rows_state_words_are_not_read_as_its_title() {
        // Exactly what the sidebar publishes for a running conversation: the
        // state, the title, then the relative age.
        assert_eq!(
            row_parts(vec![
                "进行中".into(),
                "DeepSeek 会话跳转问题".into(),
                "3分钟".into()
            ]),
            (Some("DeepSeek 会话跳转问题".into()), vec!["进行中".into()])
        );
        // A pending approval on top of a running subagent: both are states.
        assert_eq!(
            row_parts(vec![
                "等待审批".into(),
                "1 个子智能体运行中".into(),
                "手机模拟触控板控制电脑方案".into(),
                "2天".into()
            ]),
            (
                Some("手机模拟触控板控制电脑方案".into()),
                vec!["等待审批".into(), "1 个子智能体运行中".into()]
            )
        );
        // A row that renders no title at all keeps its state rather than
        // inventing one out of the age.
        assert_eq!(
            row_parts(vec!["等待审批".into(), "9小时".into()]),
            (None, vec!["等待审批".into()])
        );
        // A state that is not followed by a title is never a title either.
        assert_eq!(
            row_parts(vec!["已完成".into()]),
            (None, vec!["已完成".into()])
        );
    }

    #[test]
    fn an_age_or_a_longer_title_cannot_make_a_wrong_row_look_right() {
        let rows = vec![
            row("同步最新代码", Some("nita")),
            row("查看当前工程", Some("skills")),
        ];
        assert_eq!(match_row(&rows, &target("同步", None)), None);
        assert_eq!(match_row(&rows, &target("工程", None)), None);
        // A trailing word of the title is not a prefix of the title.
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
        // Exactly what a tray click carries, decoded the same way. The title and
        // workspace come from the environment so the smoke test can name any row
        // of the running app.
        let target = SessionTarget {
            id: "session-smoke".into(),
            title: std::env::var("AGENT_STATUS_INDICATOR_SMOKE_TITLE").ok(),
            workspace: std::env::var("AGENT_STATUS_INDICATOR_SMOKE_WORKSPACE").ok(),
        };
        let url = target.url();
        let target = crate::deepseek_jump::SessionTarget::parse(&url).expect("target");
        println!("parsed: {:?}", target);
        // Hide the window first, the way opening the tray menu does.
        let _ = std::process::Command::new("/usr/bin/open")
            .args(["-a", "Finder"])
            .status();
        sleep(Duration::from_millis(800));
        let outcome = crate::deepseek_jump::open(&url, crate::deepseek_jump::Options::default());
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
            println!("ws={:?} row={:?}", row.workspace, row);
        }
        // `AGENT_STATUS_INDICATOR_SMOKE_TITLE` lets the smoke test name a row of
        // the running app without editing the test.
        let target = SessionTarget {
            id: "session-3c4b74ef-c82e-4812-9602-0caedc4a8b87".into(),
            title: Some(
                std::env::var("AGENT_STATUS_INDICATOR_SMOKE_TITLE")
                    .unwrap_or_else(|_| "调试 APP ANR 原因".into()),
            ),
            workspace: std::env::var("AGENT_STATUS_INDICATOR_SMOKE_WORKSPACE").ok(),
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
        for age in [
            "4分钟", "2天", "11天", "24天", "1h", "3 days", "2 周", "4min", "4mo", "刚刚", "now",
            "jetzt",
        ] {
            assert!(is_relative_age(age), "{age}");
        }
        for title in [
            "2.1.70用户标签UI调整排序",
            "2024 计划",
            "同步最新代码",
            "3D 渲染",
            "调试 APP ANR 原因",
            "1 个子智能体运行中",
        ] {
            assert!(!is_relative_age(title), "{title}");
        }
    }

    #[test]
    fn only_the_sidebars_own_conversation_rows_can_be_pressed() {
        // The outline publishes workspace headers as rows too, and the transcript
        // is built from rows as well; pressing either would not open a
        // conversation.
        assert!(is_session_row(&["JoiKIa_sessionRow".into()]));
        assert!(!is_session_row(&["JoiKIa_projectRow".into()]));
        assert!(!is_session_row(&[]));
        assert!(is_project_row(&["JoiKIa_projectRow".into()]));
        assert!(!is_project_row(&["JoiKIa_sessionRow".into()]));
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
        let rows = vec![row("Fix The Build", Some("Repo"))];
        assert_eq!(match_row(&rows, &target("  fix the build ", None)), Some(0));
        assert_eq!(match_row(&rows, &target("FIX THE BUILD", None)), Some(0));
        // The projection cache and the rendered sidebar can differ in spacing
        // without naming different conversations.
        assert_eq!(
            match_row(
                &[row("FixTheBuild", Some("Repo"))],
                &target("Fix The Build", None)
            ),
            Some(0)
        );
    }
}
