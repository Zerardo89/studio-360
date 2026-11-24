use crate::online::{cache_name, emoji_urls, parse_tracks, Online};

#[test]
fn openverse_filters_licenses_and_preserves_attribution() {
    let json = br#"{"results":[
        {"title":"A","creator":"Ada","duration":2150,"license":"by","license_version":"4.0","url":"https://example.org/a.mp3","foreign_landing_url":"https://example.org/a"},
        {"title":"B","license":"by-sa","url":"https://example.org/b.mp3"},
        {"title":"C","license":"cc0","url":"https://example.org/c.mp3"},
        {"title":"D","license":"pdm","url":"https://example.org/d.mp3"},
        {"title":"E","license":"by-nc","url":"https://example.org/e.mp3"},
        {"title":"F","license":"by-nd","url":"https://example.org/f.mp3"},
        {"title":"G","license":"by","url":"file:///local.wav"}
    ]}"#;
    let tracks = parse_tracks(json).expect("catalogo");
    assert_eq!(
        tracks
            .iter()
            .map(|t| t.credit.title.as_str())
            .collect::<Vec<_>>(),
        ["A", "B", "C", "D"]
    );
    assert_eq!(tracks[0].credit.creator, "Ada");
    assert_eq!(tracks[0].credit.link, "https://example.org/a");
    assert_eq!(tracks[0].duration, 2.15);
}

#[test]
fn twemoji_uses_scalar_values_with_a_variation_selector_fallback() {
    let base = "https://cdn.jsdelivr.net/gh/jdecked/twemoji@latest/assets/72x72/";
    assert_eq!(
        emoji_urls("❤️"),
        [format!("{base}2764-fe0f.png"), format!("{base}2764.png")]
    );
    assert_eq!(emoji_urls("😀"), [format!("{base}1f600.png")]);
    assert_eq!(emoji_urls("👩‍💻"), [format!("{base}1f469-200d-1f4bb.png")]);
}

#[test]
fn cache_keys_are_stable_and_depend_on_the_full_url() {
    assert_eq!(cache_name("hello", "png"), "a430d84680aabd0b.png");
    assert_ne!(
        cache_name("https://example.org/a?q=1", "mp3"),
        cache_name("https://example.org/a?q=2", "mp3")
    );
}

#[test]
#[ignore = "richiede accesso a Openverse e jsDelivr"]
fn openverse_and_twemoji_live() {
    let online = Online::without_cache();
    let tracks = online.search("acoustic");
    let emoji = online.emoji("❤️");
    eprintln!(
        "Openverse: {:?}; Twemoji: {:?}",
        tracks.as_ref().map(Vec::len),
        emoji.as_ref().map(|img| img.dimensions())
    );
    assert!(tracks.is_ok_and(|tracks| !tracks.is_empty()));
    assert_eq!(emoji.expect("PNG Twemoji").dimensions(), (72, 72));
}
