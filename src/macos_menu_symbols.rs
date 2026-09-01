//! Native SF Symbol images for macOS menu items.
//!
//! `muda` exposes portable bitmap icons, but its macOS menu is an `NSMenu`.
//! After it builds that menu we attach SF Symbols directly to the underlying
//! `NSMenuItem`s. AppKit keeps these vector images sharp at every display
//! scale, and this cache means a symbol is created only once per name/color.

use crate::{
    browser_tab_action_label, config::Config, display_settings, i18n, notification_action_label,
    startup, startup_action_label, toggle_label,
};
use objc2::{rc::Retained, AnyThread};
use objc2_app_kit::{NSColor, NSImage, NSImageSymbolConfiguration, NSMenu};
use objc2_foundation::NSString;
use std::collections::HashMap;
use tray_icon::menu::{ContextMenu, Menu};

#[derive(Default)]
pub struct SymbolCache {
    images: HashMap<SymbolKey, Retained<NSImage>>,
    last_settings: Option<SettingsSignature>,
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct SymbolKey {
    name: &'static str,
    color: Option<[u8; 3]>,
}

#[derive(PartialEq)]
struct SettingsSignature {
    notifications_enabled: bool,
    browser_tab_reuse: bool,
    startup_enabled: bool,
    show_duration: bool,
    show_model: bool,
    show_context_percent: bool,
    show_context_used: bool,
    show_context_total: bool,
    locale: String,
}

impl SymbolCache {
    pub fn apply(&mut self, menu: &Menu, config: &Config) {
        let symbols = menu_symbols(config);
        // `Menu::ns_menu` is supplied by muda's public ContextMenu trait. The
        // menu owns this pointer for its full lifetime, and all calls happen on
        // winit's main thread as required by AppKit.
        let native_menu = unsafe { &*(menu.ns_menu().cast::<NSMenu>()) };
        self.apply_to_menu(native_menu, &symbols);
        self.last_settings = Some(SettingsSignature::new(config));
    }

    /// A status scan updates session rows every two seconds, but none of those
    /// rows use SF Symbols. Skip AppKit menu traversal unless a setting changed.
    pub fn apply_if_settings_changed(&mut self, menu: &Menu, config: &Config) {
        let settings = SettingsSignature::new(config);
        if self.last_settings.as_ref() != Some(&settings) {
            self.apply(menu, config);
        }
    }

    fn apply_to_menu(&mut self, menu: &NSMenu, symbols: &HashMap<String, SymbolKey>) {
        for item in menu.itemArray().iter() {
            let title = item.title().to_string();
            if let Some(symbol) = symbols.get(&title) {
                item.setImage(Some(self.image(symbol)));
            }
            if let Some(submenu) = item.submenu() {
                self.apply_to_menu(&submenu, symbols);
            }
        }
    }

    fn image(&mut self, key: &SymbolKey) -> &NSImage {
        self.images
            .entry(key.clone())
            .or_insert_with(|| make_symbol(key))
    }
}

impl SettingsSignature {
    fn new(config: &Config) -> Self {
        Self {
            notifications_enabled: config.notifications_enabled,
            browser_tab_reuse: config.browser_tab_reuse,
            startup_enabled: startup::is_enabled(),
            show_duration: config.show_duration,
            show_model: config.show_model,
            show_context_percent: config.show_context_percent,
            show_context_used: config.show_context_used,
            show_context_total: config.show_context_total,
            locale: config.locale.clone(),
        }
    }
}

fn make_symbol(key: &SymbolKey) -> Retained<NSImage> {
    let name = NSString::from_str(key.name);
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, None)
        // SF Symbols are available on every supported macOS release. The
        // fallback protects menu construction on unusually old systems.
        .unwrap_or_else(|| NSImage::init(NSImage::alloc()));
    let Some([red, green, blue]) = key.color else {
        return image;
    };
    let color = NSColor::colorWithSRGBRed_green_blue_alpha(
        red as f64 / 255.0,
        green as f64 / 255.0,
        blue as f64 / 255.0,
        1.0,
    );
    let configuration = NSImageSymbolConfiguration::configurationWithHierarchicalColor(&color);
    image
        .imageWithSymbolConfiguration(&configuration)
        .unwrap_or(image)
}

fn menu_symbols(config: &Config) -> HashMap<String, SymbolKey> {
    let mut symbols = HashMap::new();
    let mut add = |title: String, name, color| {
        symbols.insert(title, SymbolKey { name, color });
    };

    add(i18n::menu("settings").into(), "gearshape", None);
    add(i18n::menu("startup").into(), "power", None);
    add(i18n::menu("notifications").into(), "bell", None);
    add(i18n::menu("browser").into(), "rectangle.on.rectangle", None);
    add(i18n::menu("language").into(), "globe", None);
    add(i18n::menu("display").into(), "slider.horizontal.3", None);

    let toggle_symbol = |enabled| {
        if enabled {
            ("checkmark.circle.fill", Some([52, 199, 89]))
        } else {
            ("circle", Some([142, 142, 147]))
        }
    };
    let startup_enabled = startup::is_enabled();
    let (name, color) = toggle_symbol(startup_enabled);
    add(startup_action_label(startup_enabled).into(), name, color);

    let (name, color) = if config.notifications_enabled {
        ("bell.fill", Some([52, 199, 89]))
    } else {
        ("bell.slash", Some([142, 142, 147]))
    };
    add(
        notification_action_label(config.notifications_enabled).into(),
        name,
        color,
    );
    add(
        i18n::menu("test_notification").into(),
        "bell.badge",
        Some([0, 122, 255]),
    );
    add(i18n::text("open_notifications").into(), "gearshape", None);
    add(i18n::text("notification_app").into(), "app.badge", None);

    let (name, color) = toggle_symbol(config.browser_tab_reuse);
    add(
        browser_tab_action_label(config.browser_tab_reuse).into(),
        name,
        color,
    );
    add(i18n::menu("automation").into(), "gearshape", None);
    add(i18n::menu("permission").into(), "lock.shield", None);
    add(
        i18n::menu("install_claude").into(),
        "arrow.down.circle",
        None,
    );

    for (key, label) in display_settings() {
        let enabled = match key {
            "duration" => config.show_duration,
            "model" => config.show_model,
            "context_percent" => config.show_context_percent,
            "context_used" => config.show_context_used,
            "context_total" => config.show_context_total,
            _ => false,
        };
        let (name, color) = toggle_symbol(enabled);
        add(toggle_label(enabled, label), name, color);
    }
    symbols
}
