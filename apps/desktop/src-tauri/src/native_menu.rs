use serde::{Deserialize, Serialize};
use tauri::{AppHandle, menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder}};
#[cfg(target_os = "macos")]
use tauri::menu::PredefinedMenuItem;

#[derive(Deserialize)]
pub(crate) struct MenuSpec {
    pub(crate) title: String,
    pub(crate) items: Vec<MenuItemSpec>,
}

#[derive(Deserialize)]
pub(crate) struct MenuItemSpec {
    /// None draws a separator.
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) accel: String,
    #[serde(default)]
    pub(crate) enabled: bool,
    /// A system item of the macOS app menu: `hide`, `hide-others` or `show-all`.
    #[serde(default)]
    pub(crate) native: Option<String>,
}

/// The menu bar. The page owns the command list and sends it whole on every
/// change; a click comes back as `skiff:menu` with the item's id.
#[tauri::command]
pub(crate) fn set_menu(app: AppHandle, menus: Vec<MenuSpec>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if crate::header_bar::active() {
        return crate::header_bar::set_menu(&app, menus);
    }
    let mut bar = MenuBuilder::new(&app);
    for m in &menus {
        let mut sub = SubmenuBuilder::new(&app, &m.title);
        for it in &m.items {
            #[cfg(target_os = "macos")]
            if let Some(native) = &it.native {
                let item = match native.as_str() {
                    "hide" => PredefinedMenuItem::hide(&app, None),
                    "hide-others" => PredefinedMenuItem::hide_others(&app, None),
                    "show-all" => PredefinedMenuItem::show_all(&app, None),
                    _ => continue,
                };
                sub = sub.item(&item.map_err(|e| e.to_string())?);
                continue;
            }
            #[cfg(not(target_os = "macos"))]
            if it.native.is_some() {
                continue;
            }
            let Some(id) = &it.id else {
                sub = sub.separator();
                continue;
            };
            let item = |accel: &str| {
                let b = MenuItemBuilder::with_id(id, &it.label).enabled(it.enabled);
                if accel.is_empty() { b.build(&app) } else { b.accelerator(accel).build(&app) }
            };
            // A key the menu cannot parse stays with the page, which still handles it.
            let item = item(&it.accel).or_else(|_| item("")).map_err(|e| e.to_string())?;
            sub = sub.item(&item);
        }
        bar = bar.item(&sub.build().map_err(|e| e.to_string())?);
    }
    app.set_menu(bar.build().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct MenuLayout {
    /// The window can show a header bar: Linux only.
    available: bool,
    /// `header-bar`, `menu-bar`, or None for the automatic choice.
    chosen: Option<String>,
    /// What the automatic choice gives on this desktop.
    auto: &'static str,
    /// What the window shows now. A change shows the next time Skiff opens.
    current: &'static str,
}

fn layout_now() -> MenuLayout {
    let chosen = skiff_core::config::client::load().ok().and_then(|c| c.appearance.menu_layout);
    #[cfg(target_os = "linux")]
    let (available, auto, header) = (true, crate::header_bar::on_gnome(), crate::header_bar::active());
    #[cfg(not(target_os = "linux"))]
    let (available, auto, header) = (false, false, false);
    let name = |h: bool| if h { "header-bar" } else { "menu-bar" };
    MenuLayout { available, chosen, auto: name(auto), current: name(header) }
}

#[tauri::command]
pub(crate) fn menu_layout() -> MenuLayout {
    layout_now()
}

/// Sets `[appearance] menu_layout`. None: the automatic choice.
#[tauri::command]
pub(crate) fn set_menu_layout(layout: Option<String>) -> Result<MenuLayout, String> {
    skiff_core::config::set_menu_layout(layout.as_deref()).map_err(|e| e.to_string())?;
    Ok(layout_now())
}

/// Dark or light window chrome. Only the GNOME header bar uses it.
#[tauri::command]
pub(crate) fn set_window_dark(app: AppHandle, dark: bool) {
    #[cfg(target_os = "linux")]
    crate::header_bar::set_dark(&app, dark);
    #[cfg(not(target_os = "linux"))]
    let _ = (app, dark);
}
