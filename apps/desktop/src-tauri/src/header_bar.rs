//! The GNOME header bar: window controls, search and the main menu in the
//! title bar. It replaces the menu bar and shows the same command list.
//! `[appearance] menu_layout` picks the layout; without it, GNOME gets the
//! header bar and every other desktop keeps the menu bar.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::prelude::*;
use tauri::{AppHandle, Emitter, WebviewWindow};

use crate::native_menu::MenuSpec;

static ACTIVE: AtomicBool = AtomicBool::new(false);

thread_local! {
    static MENU_BUTTON: RefCell<Option<gtk::MenuButton>> = const { RefCell::new(None) };
}

/// True when the window shows the header bar instead of the menu bar.
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The layout the user chose, else the header bar on GNOME only.
pub(crate) fn wanted() -> bool {
    let chosen = skiff_core::config::client::load().ok().and_then(|c| c.appearance.menu_layout);
    match chosen.as_deref() {
        Some("header-bar") => true,
        Some("menu-bar") => false,
        _ => on_gnome(),
    }
}

pub(crate) fn on_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|s| s.eq_ignore_ascii_case("gnome")))
}

/// Puts the header bar on the window. GTK takes a title bar only before the
/// window first shows, so the window starts hidden.
pub(crate) fn install(app: &AppHandle, window: &WebviewWindow) -> tauri::Result<()> {
    let win = window.gtk_window()?;
    let bar = gtk::HeaderBar::new();
    bar.set_show_close_button(true);
    bar.set_title(Some("Skiff"));

    let menu = gtk::MenuButton::new();
    menu.set_image(Some(&gtk::Image::from_icon_name(Some("open-menu-symbolic"), gtk::IconSize::Button)));
    menu.set_tooltip_text(Some("Main menu"));
    menu.set_popup(Some(&gtk::Menu::new()));

    let search = gtk::Button::from_icon_name(Some("system-search-symbolic"), gtk::IconSize::Button);
    search.set_tooltip_text(Some("Search"));
    let a = app.clone();
    search.connect_clicked(move |_| {
        let _ = a.emit("skiff:search", ());
    });

    // A click on a header button must not keep the keys from the page.
    menu.set_focus_on_click(false);
    search.set_focus_on_click(false);
    // Flat, as in current GNOME apps: the frame shows on hover only.
    menu.set_relief(gtk::ReliefStyle::None);
    search.set_relief(gtk::ReliefStyle::None);

    bar.pack_end(&menu);
    bar.pack_end(&search);
    bar.show_all();
    win.set_titlebar(Some(&bar));

    // F10 opens the main menu, as in other GNOME apps.
    let m = menu.clone();
    win.connect_key_press_event(move |_, e| {
        let mods = e.state() & gtk::accelerator_get_default_mod_mask();
        if e.keyval() != gtk::gdk::keys::constants::F10 || !mods.is_empty() {
            return gtk::glib::Propagation::Proceed;
        }
        m.set_active(true);
        gtk::glib::Propagation::Stop
    });

    MENU_BUTTON.with(|b| *b.borrow_mut() = Some(menu));
    ACTIVE.store(true, Ordering::Relaxed);
    Ok(())
}

/// Rebuilds the main menu: one submenu per menu bar title.
pub(crate) fn set_menu(app: &AppHandle, menus: Vec<MenuSpec>) -> Result<(), String> {
    let a = app.clone();
    app.run_on_main_thread(move || {
        let root = gtk::Menu::new();
        for m in &menus {
            let sub = gtk::Menu::new();
            for it in &m.items {
                let Some(id) = it.id.clone() else {
                    sub.append(&gtk::SeparatorMenuItem::new());
                    continue;
                };
                let item = gtk::MenuItem::with_label(&it.label);
                item.set_sensitive(it.enabled);
                if let (Some((key, mods)), Some(label)) = (gtk_accel(&it.accel), item.child().and_then(|c| c.downcast::<gtk::AccelLabel>().ok())) {
                    label.set_accel(key, mods);
                }
                let a = a.clone();
                item.connect_activate(move |_| {
                    let _ = a.emit("skiff:menu", &id);
                });
                sub.append(&item);
            }
            let top = gtk::MenuItem::with_label(&m.title);
            top.set_submenu(Some(&sub));
            root.append(&top);
        }
        root.show_all();
        MENU_BUTTON.with(|b| {
            if let Some(b) = b.borrow().as_ref() {
                b.set_popup(Some(&root));
            }
        });
    })
    .map_err(|e| e.to_string())
}

/// The page's menu key ("CmdOrCtrl+Shift+ArrowUp") as a GTK accelerator.
/// The label only shows the key; the page handles the key press itself.
fn gtk_accel(accel: &str) -> Option<(u32, gtk::gdk::ModifierType)> {
    if accel.is_empty() {
        return None;
    }
    let mut out = String::new();
    for part in accel.split('+') {
        let p = match part {
            "CmdOrCtrl" | "Ctrl" => "<Primary>",
            "Shift" => "<Shift>",
            "Alt" => "<Alt>",
            "Enter" => "Return",
            "Equal" => "equal",
            "Minus" => "minus",
            "Comma" => "comma",
            "Space" => "space",
            k if k.starts_with("Arrow") => &k[5..],
            k if k.chars().count() == 1 => {
                out.push_str(&k.to_lowercase());
                continue;
            }
            k => k,
        };
        out.push_str(p);
    }
    let (key, mods) = gtk::accelerator_parse(&out);
    (key != 0).then_some((key, mods))
}

/// Dark or light header bar and menu, to match the app theme.
pub(crate) fn set_dark(app: &AppHandle, dark: bool) {
    if !active() {
        return;
    }
    let _ = app.run_on_main_thread(move || {
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_application_prefer_dark_theme(dark);
        }
    });
}
