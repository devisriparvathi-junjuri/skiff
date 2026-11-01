use skiff_core::config;

// Its own test binary: SKIFF_CONFIG is process-wide.
#[test]
fn font_size_round_trips_beside_the_theme() {
    let dir = std::env::temp_dir().join(format!("skiff-appearance-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("projects.toml");
    std::env::set_var("SKIFF_CONFIG", &file);

    std::fs::write(&file, "[appearance]\nfont_size = 11\n").unwrap();
    assert_eq!(config::client::load().unwrap().appearance.font_size, Some(11));

    // The first client write copies projects.toml's client tables into
    // client.toml, and writes there only: projects.toml stays as it was.
    config::set_app_theme(Some("harbor")).unwrap();
    config::set_font_size(Some(15)).unwrap();
    let a = config::client::load().unwrap().appearance;
    assert_eq!(a.theme.as_deref(), Some("harbor"));
    assert_eq!(a.font_size, Some(15));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[appearance]\nfont_size = 11\n");
    assert_eq!(config::client::path(), dir.join("client.toml"));

    config::set_font_size(None).unwrap();
    let a = config::client::load().unwrap().appearance;
    assert_eq!(a.theme.as_deref(), Some("harbor"));
    assert_eq!(a.font_size, None);

    std::fs::remove_dir_all(&dir).unwrap();
}
