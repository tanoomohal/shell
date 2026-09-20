//! Native macOS menu bar setup and action dispatching.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use winit::event_loop::EventLoopProxy;

use crate::session::{Session, UserEvent};
use alacritty_terminal::event::Event as TermEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppAction {
    // Shell (File)
    NewTab,
    CloseTab,
    CloseWindow,
    ExportText,
    ExportSelection,
    ShowInspector,
    EditTitle,
    SoftReset,
    HardReset,

    // Edit
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    PasteEscaped,
    PasteSelection,
    SelectAll,
    ClearToStart,
    ClearScrollback,
    ClearScreen,
    Find,
    FindNext,
    FindPrev,
    ToggleOptionAsMeta,

    // View
    ToggleSidebar,
    ResetFontSize,
    ZoomIn,
    ZoomOut,
    ScrollTop,
    ScrollBottom,
    PageUp,
    PageDown,
    LineUp,
    LineDown,
    ToggleFullScreen,

    // Window
    Minimize,
    Zoom,
    NextTab,
    PrevTab,
    SelectTab(usize),

    // Overlays / Tools
    CommandPalette,
    HistorySearch,
}

impl AppAction {
    pub fn tag(&self) -> isize {
        match self {
            AppAction::NewTab => 1,
            AppAction::CloseTab => 2,
            AppAction::CloseWindow => 3,
            AppAction::ExportText => 4,
            AppAction::ExportSelection => 5,
            AppAction::ShowInspector => 6,
            AppAction::EditTitle => 7,
            AppAction::SoftReset => 8,
            AppAction::HardReset => 9,

            AppAction::Undo => 10,
            AppAction::Redo => 11,
            AppAction::Cut => 12,
            AppAction::Copy => 13,
            AppAction::Paste => 14,
            AppAction::PasteEscaped => 15,
            AppAction::PasteSelection => 16,
            AppAction::SelectAll => 17,
            AppAction::ClearToStart => 18,
            AppAction::ClearScrollback => 19,
            AppAction::ClearScreen => 20,
            AppAction::Find => 21,
            AppAction::FindNext => 22,
            AppAction::FindPrev => 23,
            AppAction::ToggleOptionAsMeta => 24,

            AppAction::ToggleSidebar => 25,
            AppAction::ResetFontSize => 26,
            AppAction::ZoomIn => 27,
            AppAction::ZoomOut => 28,
            AppAction::ScrollTop => 29,
            AppAction::ScrollBottom => 30,
            AppAction::PageUp => 31,
            AppAction::PageDown => 32,
            AppAction::LineUp => 33,
            AppAction::LineDown => 34,
            AppAction::ToggleFullScreen => 35,

            AppAction::Minimize => 36,
            AppAction::Zoom => 37,
            AppAction::NextTab => 38,
            AppAction::PrevTab => 39,
            AppAction::SelectTab(n) => 41 + *n as isize,

            AppAction::CommandPalette => 50,
            AppAction::HistorySearch => 51,
        }
    }

    pub fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            1 => Some(AppAction::NewTab),
            2 => Some(AppAction::CloseTab),
            3 => Some(AppAction::CloseWindow),
            4 => Some(AppAction::ExportText),
            5 => Some(AppAction::ExportSelection),
            6 => Some(AppAction::ShowInspector),
            7 => Some(AppAction::EditTitle),
            8 => Some(AppAction::SoftReset),
            9 => Some(AppAction::HardReset),

            10 => Some(AppAction::Undo),
            11 => Some(AppAction::Redo),
            12 => Some(AppAction::Cut),
            13 => Some(AppAction::Copy),
            14 => Some(AppAction::Paste),
            15 => Some(AppAction::PasteEscaped),
            16 => Some(AppAction::PasteSelection),
            17 => Some(AppAction::SelectAll),
            18 => Some(AppAction::ClearToStart),
            19 => Some(AppAction::ClearScrollback),
            20 => Some(AppAction::ClearScreen),
            21 => Some(AppAction::Find),
            22 => Some(AppAction::FindNext),
            23 => Some(AppAction::FindPrev),
            24 => Some(AppAction::ToggleOptionAsMeta),

            25 => Some(AppAction::ToggleSidebar),
            26 => Some(AppAction::ResetFontSize),
            27 => Some(AppAction::ZoomIn),
            28 => Some(AppAction::ZoomOut),
            29 => Some(AppAction::ScrollTop),
            30 => Some(AppAction::ScrollBottom),
            31 => Some(AppAction::PageUp),
            32 => Some(AppAction::PageDown),
            33 => Some(AppAction::LineUp),
            34 => Some(AppAction::LineDown),
            35 => Some(AppAction::ToggleFullScreen),

            36 => Some(AppAction::Minimize),
            37 => Some(AppAction::Zoom),
            38 => Some(AppAction::NextTab),
            39 => Some(AppAction::PrevTab),
            41..=49 => Some(AppAction::SelectTab((tag - 41) as usize)),

            50 => Some(AppAction::CommandPalette),
            51 => Some(AppAction::HistorySearch),
            _ => None,
        }
    }
}

static PENDING_ACTIONS: Mutex<Vec<AppAction>> = Mutex::new(Vec::new());
static EVENT_PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
static MENU_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn init_proxy(proxy: EventLoopProxy<UserEvent>) {
    let _ = EVENT_PROXY.set(proxy);
}

pub fn dispatch_action(action: AppAction) {
    PENDING_ACTIONS.lock().unwrap().push(action);
    if let Some(proxy) = EVENT_PROXY.get() {
        let _ = proxy.send_event(UserEvent {
            session: u64::MAX,
            event: TermEvent::Wakeup,
        });
    }
}

pub fn take_pending_actions() -> Vec<AppAction> {
    let mut lock = PENDING_ACTIONS.lock().unwrap();
    std::mem::take(&mut *lock)
}

#[cfg(target_os = "macos")]
pub mod macos {
    use super::*;
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};

    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_allocateClassPair(
            superclass: *mut c_void,
            name: *const c_char,
            extra_bytes: usize,
        ) -> *mut c_void;
        fn class_addMethod(
            cls: *mut c_void,
            name: *mut c_void,
            imp: *mut c_void,
            types: *const c_char,
        ) -> bool;
        fn objc_registerClassPair(cls: *mut c_void);
        fn objc_msgSend();
    }

    pub unsafe fn get_class(name: &str) -> *mut c_void {
        let c_str = CString::new(name).unwrap();
        objc_getClass(c_str.as_ptr())
    }

    pub unsafe fn get_sel(name: &str) -> *mut c_void {
        let c_str = CString::new(name).unwrap();
        sel_registerName(c_str.as_ptr())
    }

    pub unsafe fn nsstring(s: &str) -> *mut c_void {
        let cls = get_class("NSString");
        let sel = get_sel("stringWithUTF8String:");
        let c_str = CString::new(s).unwrap();
        let msg_send: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        msg_send(cls, sel, c_str.as_ptr())
    }

    extern "C" fn menu_action_callback(
        _this: *mut c_void,
        _cmd: *mut c_void,
        sender: *mut c_void,
    ) {
        unsafe {
            let sel = get_sel("tag");
            let msg_send: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
                std::mem::transmute(objc_msgSend as *const ());
            let tag = msg_send(sender, sel);
            if let Some(action) = AppAction::from_tag(tag) {
                dispatch_action(action);
            }
        }
    }

    pub unsafe fn get_or_create_menu_target() -> *mut c_void {
        let cls_name = CString::new("ShellNativeMenuTarget").unwrap();
        let mut cls = objc_getClass(cls_name.as_ptr());
        if cls.is_null() {
            let superclass = get_class("NSObject");
            cls = objc_allocateClassPair(superclass, cls_name.as_ptr(), 0);
            let sel = get_sel("menuAction:");
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel,
                menu_action_callback as *const () as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
        }

        let alloc_sel = get_sel("alloc");
        let init_sel = get_sel("init");
        let msg_alloc: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let raw = msg_alloc(cls, alloc_sel);
        let msg_init: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        msg_init(raw, init_sel)
    }

    pub const CMD: usize = 1 << 20;
    pub const CMD_SHIFT: usize = (1 << 20) | (1 << 17);
    pub const CMD_OPT: usize = (1 << 20) | (1 << 19);
    pub const CMD_CTRL: usize = (1 << 20) | (1 << 18);
    pub const CMD_OPT_CTRL: usize = (1 << 20) | (1 << 19) | (1 << 18);

    pub unsafe fn add_menu_item(
        menu: *mut c_void,
        title: &str,
        key_equiv: &str,
        modifiers: usize,
        action_opt: Option<*mut c_void>,
        tag: isize,
        target: *mut c_void,
    ) -> *mut c_void {
        let item_cls = get_class("NSMenuItem");
        let alloc_sel = get_sel("alloc");
        let init_sel = get_sel("initWithTitle:action:keyEquivalent:");

        let msg_alloc: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let raw_item = msg_alloc(item_cls, alloc_sel);

        let title_str = nsstring(title);
        let key_str = nsstring(key_equiv);
        let sel = action_opt.unwrap_or_else(|| get_sel("menuAction:"));

        let msg_init: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *mut c_void,
        ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
        let item = msg_init(raw_item, init_sel, title_str, sel, key_str);

        if modifiers != 0 {
            let set_mask_sel = get_sel("setKeyEquivalentModifierMask:");
            let msg_mask: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) =
                std::mem::transmute(objc_msgSend as *const ());
            msg_mask(item, set_mask_sel, modifiers);
        }

        if tag != 0 {
            let set_tag_sel = get_sel("setTag:");
            let msg_tag: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) =
                std::mem::transmute(objc_msgSend as *const ());
            msg_tag(item, set_tag_sel, tag);
        }

        if !target.is_null() && action_opt.is_none() {
            let set_target_sel = get_sel("setTarget:");
            let msg_target: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                std::mem::transmute(objc_msgSend as *const ());
            msg_target(item, set_target_sel, target);
        }

        let add_item_sel = get_sel("addItem:");
        let msg_add: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        msg_add(menu, add_item_sel, item);

        item
    }

    pub unsafe fn add_separator(menu: *mut c_void) {
        let item_cls = get_class("NSMenuItem");
        let sep_sel = get_sel("separatorItem");
        let msg_sep: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let sep = msg_sep(item_cls, sep_sel);

        let add_item_sel = get_sel("addItem:");
        let msg_add: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        msg_add(menu, add_item_sel, sep);
    }

    pub unsafe fn create_submenu(parent_menu: *mut c_void, title: &str) -> *mut c_void {
        let menu_cls = get_class("NSMenu");
        let alloc_sel = get_sel("alloc");
        let init_sel = get_sel("initWithTitle:");

        let msg_alloc: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let raw_menu = msg_alloc(menu_cls, alloc_sel);

        let title_str = nsstring(title);
        let msg_init: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let submenu = msg_init(raw_menu, init_sel, title_str);

        let item_cls = get_class("NSMenuItem");
        let raw_item = msg_alloc(item_cls, alloc_sel);
        let init_item_sel = get_sel("initWithTitle:action:keyEquivalent:");
        let empty_key = nsstring("");
        let msg_init_item: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *mut c_void,
        ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
        let item = msg_init_item(
            raw_item,
            init_item_sel,
            title_str,
            std::ptr::null_mut(),
            empty_key,
        );

        let set_submenu_sel = get_sel("setSubmenu:");
        let msg_set_sub: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        msg_set_sub(item, set_submenu_sel, submenu);

        let add_item_sel = get_sel("addItem:");
        let msg_add: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            std::mem::transmute(objc_msgSend as *const ());
        msg_add(parent_menu, add_item_sel, item);

        submenu
    }

    static WINDOW_MENU_PTR: Mutex<Option<usize>> = Mutex::new(None);
    static TARGET_PTR: Mutex<Option<usize>> = Mutex::new(None);

    pub fn setup_menu_bar() {
        if MENU_INITIALIZED.swap(true, Ordering::SeqCst) {
            return;
        }

        unsafe {
            let target = get_or_create_menu_target();
            *TARGET_PTR.lock().unwrap() = Some(target as usize);

            let app_cls = get_class("NSApplication");
            let shared_app_sel = get_sel("sharedApplication");
            let msg_app: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let app = msg_app(app_cls, shared_app_sel);

            let menu_cls = get_class("NSMenu");
            let alloc_sel = get_sel("alloc");
            let init_sel = get_sel("initWithTitle:");
            let msg_alloc: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let raw_main = msg_alloc(menu_cls, alloc_sel);
            let msg_init: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let main_menu = msg_init(raw_main, init_sel, nsstring("MainMenu"));

            // 1. App Menu: Shell
            let app_menu = create_submenu(main_menu, "Shell");
            add_menu_item(app_menu, "About Shell", "", 0, Some(get_sel("orderFrontStandardAboutPanel:")), 0, target);
            add_separator(app_menu);
            add_menu_item(app_menu, "Hide Shell", "h", CMD, Some(get_sel("hide:")), 0, target);
            add_menu_item(app_menu, "Hide Others", "h", CMD_OPT, Some(get_sel("hideOtherApplications:")), 0, target);
            add_menu_item(app_menu, "Show All", "", 0, Some(get_sel("unhideAllApplications:")), 0, target);
            add_separator(app_menu);
            add_menu_item(app_menu, "Quit Shell", "q", CMD, Some(get_sel("terminate:")), 0, target);

            // 2. Shell Menu (File - Screenshot 2)
            let shell_menu = create_submenu(main_menu, "Shell");
            add_menu_item(shell_menu, "New Tab", "t", CMD, None, AppAction::NewTab.tag(), target);
            add_menu_item(shell_menu, "Close Tab", "w", CMD, None, AppAction::CloseTab.tag(), target);
            add_menu_item(shell_menu, "Close Window", "w", CMD_SHIFT, None, AppAction::CloseWindow.tag(), target);
            add_separator(shell_menu);
            add_menu_item(shell_menu, "Export Text As...", "s", CMD, None, AppAction::ExportText.tag(), target);
            add_menu_item(shell_menu, "Export Selected Text As...", "s", CMD_SHIFT, None, AppAction::ExportSelection.tag(), target);
            add_separator(shell_menu);
            add_menu_item(shell_menu, "Show Inspector", "i", CMD, None, AppAction::ShowInspector.tag(), target);
            add_menu_item(shell_menu, "Edit Title...", "i", CMD_SHIFT, None, AppAction::EditTitle.tag(), target);
            add_separator(shell_menu);
            add_menu_item(shell_menu, "Reset Terminal", "r", CMD_OPT, None, AppAction::SoftReset.tag(), target);
            add_menu_item(shell_menu, "Hard Reset", "r", CMD_OPT_CTRL, None, AppAction::HardReset.tag(), target);

            // 3. Edit Menu (Screenshot 1)
            let edit_menu = create_submenu(main_menu, "Edit");
            add_menu_item(edit_menu, "Undo", "z", CMD, None, AppAction::Undo.tag(), target);
            add_menu_item(edit_menu, "Redo", "z", CMD_SHIFT, None, AppAction::Redo.tag(), target);
            add_separator(edit_menu);
            add_menu_item(edit_menu, "Cut", "x", CMD, None, AppAction::Cut.tag(), target);
            add_menu_item(edit_menu, "Copy", "c", CMD, None, AppAction::Copy.tag(), target);
            add_menu_item(edit_menu, "Paste", "v", CMD, None, AppAction::Paste.tag(), target);
            add_menu_item(edit_menu, "Paste Escaped Text", "v", CMD_CTRL, None, AppAction::PasteEscaped.tag(), target);
            add_menu_item(edit_menu, "Paste Selection", "v", CMD_SHIFT, None, AppAction::PasteSelection.tag(), target);
            add_menu_item(edit_menu, "Select All", "a", CMD, None, AppAction::SelectAll.tag(), target);
            add_separator(edit_menu);
            add_menu_item(edit_menu, "Clear to Start", "k", CMD, None, AppAction::ClearToStart.tag(), target);
            add_menu_item(edit_menu, "Clear Scrollback", "k", CMD_OPT, None, AppAction::ClearScrollback.tag(), target);
            add_menu_item(edit_menu, "Clear Screen", "l", CMD_CTRL, None, AppAction::ClearScreen.tag(), target);
            add_separator(edit_menu);
            add_menu_item(edit_menu, "Find", "f", CMD, None, AppAction::Find.tag(), target);
            add_menu_item(edit_menu, "Find Next", "g", CMD, None, AppAction::FindNext.tag(), target);
            add_menu_item(edit_menu, "Find Previous", "g", CMD_SHIFT, None, AppAction::FindPrev.tag(), target);
            add_separator(edit_menu);
            add_menu_item(edit_menu, "Use Option as Meta Key", "o", CMD_OPT, None, AppAction::ToggleOptionAsMeta.tag(), target);

            // 4. View Menu (Screenshot 3)
            let view_menu = create_submenu(main_menu, "View");
            add_menu_item(view_menu, "Toggle Sidebar", "t", CMD_SHIFT, None, AppAction::ToggleSidebar.tag(), target);
            add_separator(view_menu);
            add_menu_item(view_menu, "Default Font Size", "0", CMD, None, AppAction::ResetFontSize.tag(), target);
            add_menu_item(view_menu, "Bigger", "+", CMD, None, AppAction::ZoomIn.tag(), target);
            add_menu_item(view_menu, "Smaller", "-", CMD, None, AppAction::ZoomOut.tag(), target);
            add_separator(view_menu);
            add_menu_item(view_menu, "Scroll to Top", "\u{F72C}", CMD, None, AppAction::ScrollTop.tag(), target);
            add_menu_item(view_menu, "Scroll to Bottom", "\u{F72D}", CMD, None, AppAction::ScrollBottom.tag(), target);
            add_menu_item(view_menu, "Page Up", "\u{F700}", CMD, None, AppAction::PageUp.tag(), target);
            add_menu_item(view_menu, "Page Down", "\u{F701}", CMD, None, AppAction::PageDown.tag(), target);
            add_menu_item(view_menu, "Line Up", "\u{F700}", CMD_OPT, None, AppAction::LineUp.tag(), target);
            add_menu_item(view_menu, "Line Down", "\u{F701}", CMD_OPT, None, AppAction::LineDown.tag(), target);
            add_separator(view_menu);
            add_menu_item(view_menu, "Enter Full Screen", "f", CMD_CTRL, None, AppAction::ToggleFullScreen.tag(), target);

            // 5. Window Menu (Screenshot 4)
            let window_menu = create_submenu(main_menu, "Window");
            *WINDOW_MENU_PTR.lock().unwrap() = Some(window_menu as usize);
            add_menu_item(window_menu, "Minimize", "m", CMD, Some(get_sel("performMiniaturize:")), 0, target);
            add_menu_item(window_menu, "Zoom", "", 0, Some(get_sel("performZoom:")), 0, target);
            add_separator(window_menu);
            add_menu_item(window_menu, "Show Previous Tab", "[", CMD, None, AppAction::PrevTab.tag(), target);
            add_menu_item(window_menu, "Show Next Tab", "]", CMD, None, AppAction::NextTab.tag(), target);
            add_separator(window_menu);

            // 6. Help Menu
            let help_menu = create_submenu(main_menu, "Help");
            add_menu_item(help_menu, "Command Palette...", "p", CMD, None, AppAction::CommandPalette.tag(), target);
            add_menu_item(help_menu, "Fuzzy History Search...", "r", CMD, None, AppAction::HistorySearch.tag(), target);

            let set_main_menu_sel = get_sel("setMainMenu:");
            let msg_set_main: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                std::mem::transmute(objc_msgSend as *const ());
            msg_set_main(app, set_main_menu_sel, main_menu);
        }
    }

    pub fn update_window_tabs(sessions: &[Session], active_index: usize) {
        let window_menu = *WINDOW_MENU_PTR.lock().unwrap();
        let target = *TARGET_PTR.lock().unwrap();
        let (Some(menu_usize), Some(target_usize)) = (window_menu, target) else {
            return;
        };

        unsafe {
            let menu = menu_usize as *mut c_void;
            let target_ptr = target_usize as *mut c_void;

            let items_sel = get_sel("itemArray");
            let msg_items: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let item_array = msg_items(menu, items_sel);

            let count_sel = get_sel("count");
            let msg_count: unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize =
                std::mem::transmute(objc_msgSend as *const ());
            let count = msg_count(item_array, count_sel);

            let item_at_sel = get_sel("objectAtIndex:");
            let remove_item_sel = get_sel("removeItem:");
            let tag_sel = get_sel("tag");

            let mut to_remove = Vec::new();
            for i in 0..count {
                let msg_at: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void =
                    std::mem::transmute(objc_msgSend as *const ());
                let item = msg_at(item_array, item_at_sel, i);

                let msg_tag: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
                    std::mem::transmute(objc_msgSend as *const ());
                let tag = msg_tag(item, tag_sel);
                if (41..=49).contains(&tag) {
                    to_remove.push(item);
                }
            }

            for item in to_remove {
                let msg_rem: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
                    std::mem::transmute(objc_msgSend as *const ());
                msg_rem(menu, remove_item_sel, item);
            }

            // Append current tabs formatted exactly as in Screenshot 4:
            // e.g. "tanoomohal — agy — 147x63" with shortcut ⌥⌘1..9
            let user = whoami_user();
            for (idx, session) in sessions.iter().enumerate().take(9) {
                let proc_name = session.label();
                let size = format!("{}x{}", session.size.columns, session.size.screen_lines);
                let check_prefix = if idx == active_index {
                    "✓ "
                } else if session.needs_attention {
                    "● "
                } else {
                    "  "
                };
                let title = format!("{check_prefix}{user} — {proc_name} — {size}");
                let key_digit = (idx + 1).to_string();
                let tag = 41 + idx as isize;
                add_menu_item(menu, &title, &key_digit, CMD_OPT, None, tag, target_ptr);
            }
        }
    }
}

pub fn whoami_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_string())
}

#[cfg(not(target_os = "macos"))]
pub mod macos {
    use super::*;
    pub fn setup_menu_bar() {}
    pub fn update_window_tabs(_sessions: &[Session], _active_index: usize) {}
}

pub fn setup_menu_bar() {
    macos::setup_menu_bar();
}

pub fn update_window_tabs(sessions: &[Session], active_index: usize) {
    macos::update_window_tabs(sessions, active_index);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_action_tags() {
        let actions = [
            AppAction::NewTab,
            AppAction::CloseTab,
            AppAction::CloseWindow,
            AppAction::ExportText,
            AppAction::ExportSelection,
            AppAction::ShowInspector,
            AppAction::EditTitle,
            AppAction::SoftReset,
            AppAction::HardReset,
            AppAction::Undo,
            AppAction::Redo,
            AppAction::Cut,
            AppAction::Copy,
            AppAction::Paste,
            AppAction::PasteEscaped,
            AppAction::PasteSelection,
            AppAction::SelectAll,
            AppAction::ClearToStart,
            AppAction::ClearScrollback,
            AppAction::ClearScreen,
            AppAction::Find,
            AppAction::FindNext,
            AppAction::FindPrev,
            AppAction::ToggleOptionAsMeta,
            AppAction::ToggleSidebar,
            AppAction::ResetFontSize,
            AppAction::ZoomIn,
            AppAction::ZoomOut,
            AppAction::ScrollTop,
            AppAction::ScrollBottom,
            AppAction::PageUp,
            AppAction::PageDown,
            AppAction::LineUp,
            AppAction::LineDown,
            AppAction::ToggleFullScreen,
            AppAction::Minimize,
            AppAction::Zoom,
            AppAction::NextTab,
            AppAction::PrevTab,
            AppAction::SelectTab(0),
            AppAction::SelectTab(8),
            AppAction::CommandPalette,
            AppAction::HistorySearch,
        ];

        for action in actions {
            let tag = action.tag();
            let recovered = AppAction::from_tag(tag).expect("action roundtrip failed");
            assert_eq!(action, recovered);
        }
    }
}
