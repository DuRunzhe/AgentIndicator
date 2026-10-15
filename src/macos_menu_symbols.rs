//! Native SF Symbol images for macOS menu items.
//!
//! `muda` exposes portable bitmap icons, but its macOS menu is an `NSMenu`.
//! After it builds that menu we attach SF Symbols directly to the underlying
//! `NSMenuItem`s. AppKit keeps these vector images sharp at every display
//! scale, and this cache means a symbol is created only once per name/color.

use crate::{
    browser_tab_action_label, claude_action_label, claude_statusline,
    config::{Config, CONVERSATION_WINDOWS},
    display_settings, i18n, notification_action_label, notification_preferences, startup,
    startup_action_label, toggle_label,
};
use objc2::{rc::Retained, runtime::AnyObject, AnyThread};
use objc2_app_kit::{
    NSColor, NSFont, NSFontAttributeName, NSImage, NSImageSymbolConfiguration, NSMenu,
};
use objc2_foundation::{NSAttributedString, NSAttributedStringKey, NSDictionary, NSString};
use std::collections::HashMap;
use tray_icon::menu::{ContextMenu, Menu};

/// Draw the given top-level rows in a monospaced font so the resource summary's
/// columns line up. Space padding alone cannot align them, because the menu's
/// proportional font gives a name and its value different pixel widths.
pub fn apply_monospaced(menu: &Menu, titles: &[String]) {
    // `Menu::ns_menu` is supplied by muda's public ContextMenu trait; see
    // `SymbolCache::apply` for the lifetime and main-thread reasoning.
    let native_menu = unsafe { &*(menu.ns_menu().cast::<NSMenu>()) };
    let size = NSFont::systemFontSize();
    let font =
        NSFont::userFixedPitchFontOfSize(size).unwrap_or_else(|| NSFont::systemFontOfSize(size));
    // Reading an extern static is unsafe; the symbol is always present on the
    // platforms this binary supports.
    let key = unsafe { NSFontAttributeName };
    let attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
        NSDictionary::from_retained_objects(&[key], &[font.into()]);
    for item in native_menu.itemArray().iter() {
        let title = item.title().to_string();
        if !titles.iter().any(|candidate| *candidate == title) {
            continue;
        }
        let attributed = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(&title),
                Some(&attributes),
            )
        };
        item.setAttributedTitle(Some(&attributed));
    }
}

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

/// One menu entry, identified by the submenu that owns it and its title.
#[derive(Clone, Hash, Eq, PartialEq)]
struct SymbolTarget {
    owner: String,
    item: String,
}

#[derive(Debug, PartialEq)]
struct SettingsSignature {
    notifications_enabled: bool,
    browser_tab_reuse: bool,
    startup_enabled: bool,
    claude_installed: bool,
    show_duration: bool,
    show_model: bool,
    show_context_percent: bool,
    show_context_used: bool,
    show_context_total: bool,
    show_cpu: bool,
    show_memory: bool,
    show_resources: bool,
    show_stopped_agents: bool,
    locale: String,
    conversation_window: String,
    deepseek_desktop_window: String,
}

impl SymbolCache {
    pub fn apply(&mut self, menu: &Menu, config: &Config) {
        let symbols = menu_symbols(config);
        // `Menu::ns_menu` is supplied by muda's public ContextMenu trait. The
        // menu owns this pointer for its full lifetime, and all calls happen on
        // winit's main thread as required by AppKit.
        let native_menu = unsafe { &*(menu.ns_menu().cast::<NSMenu>()) };
        self.apply_to_menu(native_menu, "", &symbols);
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

    fn apply_to_menu(
        &mut self,
        menu: &NSMenu,
        owner: &str,
        symbols: &HashMap<SymbolTarget, SymbolKey>,
    ) {
        for item in menu.itemArray().iter() {
            let title = item.title().to_string();
            // The owner (the enclosing submenu's title) is part of the lookup
            // key: the hosted and DeepSeek ranges offer the same five labels, so
            // matching on the title alone made the two groups share one check
            // mark and the last one written won.
            let target = SymbolTarget {
                owner: owner.to_owned(),
                item: title,
            };
            if let Some(symbol) = symbols.get(&target) {
                item.setImage(Some(self.image(symbol)));
            }
            // AppKit's own submenu, so the recursion stays in one object model
            // and can name the owner of the entries it is about to visit.
            if let Some(submenu) = item.submenu() {
                let name = submenu.title().to_string();
                self.apply_to_menu(&submenu, &name, symbols);
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
            claude_installed: claude_statusline::is_installed(),
            show_duration: config.show_duration,
            show_model: config.show_model,
            show_context_percent: config.show_context_percent,
            show_context_used: config.show_context_used,
            show_context_total: config.show_context_total,
            show_cpu: config.show_cpu,
            show_memory: config.show_memory,
            show_resources: config.show_resources,
            show_stopped_agents: config.show_stopped_agents,
            locale: config.locale.clone(),
            conversation_window: config.conversation_window.clone(),
            deepseek_desktop_window: config.deepseek_desktop_window.clone(),
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

fn menu_symbols(config: &Config) -> HashMap<SymbolTarget, SymbolKey> {
    let mut symbols: HashMap<SymbolTarget, SymbolKey> = HashMap::new();
    // Every entry is keyed by its owning submenu's title plus its own title.
    // An empty owner means a top-level item (only the Settings submenu today);
    // a row nested inside a submenu must register that submenu's title, or the
    // AppKit walk in `apply_to_menu` will not find it. The conversation ranges
    // share their five labels, so this owner is also what keeps the hosted and
    // DeepSeek checks from overwriting each other.
    fn add(
        symbols: &mut HashMap<SymbolTarget, SymbolKey>,
        owner: &str,
        title: String,
        name: &'static str,
        color: Option<[u8; 3]>,
    ) {
        symbols.insert(
            SymbolTarget {
                owner: owner.to_owned(),
                item: title,
            },
            SymbolKey { name, color },
        );
    }

    let settings_owner = i18n::menu("settings");
    let startup_owner = i18n::menu("startup");
    let notifications_owner = i18n::menu("notifications");
    let browser_owner = i18n::menu("browser");
    let language_owner = i18n::menu("language");
    let display_owner = i18n::menu("display");
    let hosted_owner = i18n::menu("chatgpt_window");
    let deepseek_owner = i18n::menu("deepseek_window");

    add(&mut symbols, "", settings_owner.into(), "gearshape", None);
    // Submenu titles are rows of their parent, so each one is owned by the
    // submenu it is appended to (every one of these lives under Settings).
    add(
        &mut symbols,
        settings_owner,
        startup_owner.into(),
        "power",
        None,
    );
    add(
        &mut symbols,
        settings_owner,
        notifications_owner.into(),
        "bell",
        None,
    );
    add(
        &mut symbols,
        settings_owner,
        browser_owner.into(),
        "rectangle.on.rectangle",
        None,
    );
    add(
        &mut symbols,
        settings_owner,
        hosted_owner.into(),
        "clock.arrow.circlepath",
        None,
    );
    add(
        &mut symbols,
        settings_owner,
        deepseek_owner.into(),
        "clock.arrow.circlepath",
        None,
    );
    add(
        &mut symbols,
        settings_owner,
        language_owner.into(),
        "globe",
        None,
    );
    add(
        &mut symbols,
        settings_owner,
        display_owner.into(),
        "slider.horizontal.3",
        None,
    );

    let toggle_symbol = |enabled| {
        if enabled {
            ("checkmark.circle.fill", Some([52, 199, 89]))
        } else {
            ("circle", Some([142, 142, 147]))
        }
    };
    let startup_enabled = startup::is_enabled();
    let (name, color) = toggle_symbol(startup_enabled);
    add(
        &mut symbols,
        startup_owner,
        startup_action_label(startup_enabled).into(),
        name,
        color,
    );

    let (name, color) = if config.notifications_enabled {
        ("bell.fill", Some([52, 199, 89]))
    } else {
        ("bell.slash", Some([142, 142, 147]))
    };
    add(
        &mut symbols,
        notifications_owner,
        notification_action_label(config.notifications_enabled).into(),
        name,
        color,
    );
    add(
        &mut symbols,
        notifications_owner,
        i18n::menu("test_notification").into(),
        "bell.badge",
        Some([0, 122, 255]),
    );
    add(
        &mut symbols,
        notifications_owner,
        i18n::text("open_notifications").into(),
        "gearshape",
        None,
    );
    add(
        &mut symbols,
        notifications_owner,
        i18n::text("notification_app").into(),
        "app.badge",
        None,
    );

    let (name, color) = toggle_symbol(config.browser_tab_reuse);
    add(
        &mut symbols,
        browser_owner,
        browser_tab_action_label(config.browser_tab_reuse).into(),
        name,
        color,
    );
    add(
        &mut symbols,
        browser_owner,
        i18n::menu("automation").into(),
        "gearshape",
        None,
    );
    add(
        &mut symbols,
        browser_owner,
        i18n::menu("permission").into(),
        "lock.shield",
        None,
    );
    // The row flips between install and uninstall, so its symbol tracks the
    // live collector state and is refreshed whenever that state changes.
    let claude_installed = claude_statusline::is_installed();
    let (name, color) = toggle_symbol(claude_installed);
    add(
        &mut symbols,
        settings_owner,
        claude_action_label(claude_installed).into(),
        name,
        color,
    );

    for (key, label) in display_settings() {
        let enabled = match key {
            "duration" => config.show_duration,
            "model" => config.show_model,
            "context_percent" => config.show_context_percent,
            "context_used" => config.show_context_used,
            "context_total" => config.show_context_total,
            "cpu" => config.show_cpu,
            "memory" => config.show_memory,
            "resources" => config.show_resources,
            "stopped_agents" => config.show_stopped_agents,
            _ => false,
        };
        let (name, color) = toggle_symbol(enabled);
        add(
            &mut symbols,
            display_owner,
            toggle_label(enabled, label),
            name,
            color,
        );
    }
    for (_, label, enabled) in notification_preferences(config) {
        let (name, color) = toggle_symbol(enabled);
        add(
            &mut symbols,
            notifications_owner,
            toggle_label(enabled, label),
            name,
            color,
        );
    }
    for value in i18n::LANGUAGES {
        let selected = config.locale == value;
        let (name, color) = toggle_symbol(selected);
        add(
            &mut symbols,
            language_owner,
            i18n::language_name(value),
            name,
            color,
        );
    }
    for (key, _) in CONVERSATION_WINDOWS {
        let (name, color) = toggle_symbol(config.conversation_window == key);
        add(
            &mut symbols,
            hosted_owner,
            i18n::conversation_window_label(key).into(),
            name,
            color,
        );
    }
    for (key, _) in CONVERSATION_WINDOWS {
        let (name, color) = toggle_symbol(config.deepseek_desktop_window == key);
        add(
            &mut symbols,
            deepseek_owner,
            i18n::conversation_window_label(key).into(),
            name,
            color,
        );
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The symbol recorded for one entry of one range submenu.
    fn range_symbol(config: &Config, owner: &str, key: &str) -> Option<&'static str> {
        menu_symbols(config)
            .get(&SymbolTarget {
                owner: owner.to_owned(),
                item: i18n::conversation_window_label(key).to_owned(),
            })
            .map(|symbol| symbol.name)
    }

    #[test]
    fn each_range_submenu_checks_its_own_selection() {
        let mut config = Config {
            conversation_window: "12h".into(),
            deepseek_desktop_window: "15m".into(),
            ..Default::default()
        };
        let hosted = i18n::menu("chatgpt_window");
        let deepseek = i18n::menu("deepseek_window");
        for (key, _) in CONVERSATION_WINDOWS {
            assert_eq!(
                range_symbol(&config, hosted, key),
                Some(if key == "12h" {
                    "checkmark.circle.fill"
                } else {
                    "circle"
                }),
                "hosted {key}"
            );
            assert_eq!(
                range_symbol(&config, deepseek, key),
                Some(if key == "15m" {
                    "checkmark.circle.fill"
                } else {
                    "circle"
                }),
                "deepseek {key}"
            );
        }
        // The two ranges must be able to differ: with a shared lookup key the
        // second group overwrote the first and both showed the same check.
        assert_ne!(
            range_symbol(&config, hosted, "15m"),
            range_symbol(&config, deepseek, "15m")
        );

        // Moving one selection moves only that range's check mark.
        config.conversation_window = "all".into();
        assert_eq!(
            range_symbol(&config, hosted, "all"),
            Some("checkmark.circle.fill")
        );
        assert_eq!(range_symbol(&config, hosted, "12h"), Some("circle"));
        assert_eq!(
            range_symbol(&config, deepseek, "15m"),
            Some("checkmark.circle.fill"),
            "the DeepSeek range must not follow the hosted one"
        );
    }

    /// Every row inside the Settings submenu must be keyed by that submenu's
    /// title. A regression made `add` hard-code an empty owner, so only the
    /// two range submenus (which used a different helper) kept their icons and
    /// every other nested row silently lost its check mark or header symbol.
    #[test]
    fn every_settings_row_is_keyed_by_its_owning_submenu() {
        let config = Config::default();
        let symbols = menu_symbols(&config);
        let settings = i18n::menu("settings");

        let assert_keyed = |owner: &str, item: &str| {
            assert!(
                symbols.contains_key(&SymbolTarget {
                    owner: owner.to_owned(),
                    item: item.to_owned(),
                }),
                "missing symbol for owner {owner:?} item {item:?}"
            );
        };

        // Top-level Settings header, then the submenu headers it contains.
        assert_keyed("", settings);
        for owner in [
            i18n::menu("startup"),
            i18n::menu("notifications"),
            i18n::menu("browser"),
            i18n::menu("chatgpt_window"),
            i18n::menu("deepseek_window"),
            i18n::menu("language"),
            i18n::menu("display"),
        ] {
            assert_keyed(settings, owner);
        }

        // Rows nested one level deeper, owned by their own submenu.
        assert_keyed(
            settings,
            claude_action_label(claude_statusline::is_installed()),
        );
        assert_keyed(
            i18n::menu("startup"),
            startup_action_label(startup::is_enabled()),
        );
        for item in [
            notification_action_label(config.notifications_enabled),
            i18n::menu("test_notification"),
            i18n::text("open_notifications"),
            i18n::text("notification_app"),
        ] {
            assert_keyed(i18n::menu("notifications"), item);
        }
        for item in [
            browser_tab_action_label(config.browser_tab_reuse),
            i18n::menu("automation"),
            i18n::menu("permission"),
        ] {
            assert_keyed(i18n::menu("browser"), item);
        }
        for (_, label) in display_settings() {
            assert_keyed(i18n::menu("display"), label);
        }
        for (_, label, _) in notification_preferences(&config) {
            assert_keyed(i18n::menu("notifications"), label);
        }
        for value in i18n::LANGUAGES {
            assert_keyed(i18n::menu("language"), &i18n::language_name(value));
        }
    }

    #[test]
    fn settings_signature_tracks_the_conversation_window() {
        let before = Config::default();
        let after = Config {
            conversation_window: "1h".into(),
            ..Default::default()
        };
        assert_ne!(
            SettingsSignature::new(&before),
            SettingsSignature::new(&after),
            "changing the window must re-apply the menu symbols"
        );
    }
}
