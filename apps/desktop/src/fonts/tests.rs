use super::*;
use rebook_layout::{LayoutEngine, ReaderFontFamilies, ReaderTypography};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    thread,
};

fn root() -> PathBuf {
    std::env::temp_dir().join(format!("torto-fonts-test-{}", uuid::Uuid::new_v4()))
}
fn fixture(index: usize) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts")
        .join(&catalog::catalog().fonts[index].file);
    fs::read(path).unwrap()
}

#[test]
fn pinned_assets_register_both_reader_families_and_true_italic() {
    let mut set = FontSet::default();
    for (index, asset) in catalog::catalog().fonts.iter().enumerate() {
        set.faces[index] = Some(store::validate(asset, fixture(index), true).unwrap());
    }
    let mut engine = LayoutEngine::with_fonts(set.reader_fonts().iter().cloned());
    let families = engine.available_reader_font_families();
    assert!(families.serif.iter().any(|family| family == "Literata"));
    assert!(
        families
            .chinese
            .iter()
            .any(|family| family == "LXGW WenKai GB Screen")
    );
    let mut wrong_style = catalog::catalog().fonts[2].clone();
    wrong_style.italic = false;
    assert!(store::validate(&wrong_style, fixture(2), false).is_err());
}

#[test]
fn downloaded_defaults_keep_preferences_while_offline_and_repair_unsafe_choices() {
    let mut families = ReaderFontFamilies::default();
    families.include_downloadable_defaults();
    let mut preferences = ReaderTypography::default();
    preferences.cjk_default_font = Some(rebook_layout::ReaderFontChoice {
        category: rebook_layout::ReaderDefaultFont::Serif,
        family: "Literata".into(),
    });
    preferences.latin_cjk_font = Some("LXGW WenKai GB Screen".into());
    let before = preferences.clone();
    assert!(!families.repair_typography(&mut preferences));
    assert_eq!(preferences, before);
    preferences.default_cjk_font = "SimSun".into();
    assert!(families.repair_typography(&mut preferences));
    assert_eq!(preferences.default_cjk_font, "LXGW WenKai GB Screen");
}

#[test]
fn invalid_or_partial_download_never_replaces_a_valid_persistent_font() {
    let root = root();
    let asset = &catalog::catalog().fonts[1];
    let valid = fixture(1);
    store::save(&root, asset, valid.clone()).unwrap();
    let path = store::path(&root, asset);
    let mut corrupted = valid.clone();
    corrupted[500] ^= 1;
    assert!(store::save(&root, asset, corrupted).is_err());
    assert!(store::save(&root, asset, valid[..100].to_vec()).is_err());
    assert_eq!(fs::read(&path).unwrap(), valid);
    fs::write(path.with_extension("part"), b"interrupted").unwrap();
    assert!(store::load_cached(&root, asset).is_some());
    fs::write(&path, b"interrupted").unwrap();
    assert!(store::load_cached(&root, asset).is_none());
    fs::remove_dir_all(root).unwrap();
}

fn server(body: Vec<u8>, declared_size: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut request = [0; 4096];
        let length = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET /font HTTP/1.1"));
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {declared_size}\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    });
    (format!("http://{address}/font"), handle)
}

#[test]
fn download_validates_and_reuses_disk_without_a_server_after_restart() {
    let root = root();
    let asset = &catalog::catalog().fonts[1];
    let (url, server) = server(fixture(1), asset.size);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = download::client().unwrap();
    let mut progress = Vec::new();
    let bytes = runtime
        .block_on(download::fetch_url(
            &client,
            &url,
            asset.size,
            &mut |percent| progress.push(percent),
        ))
        .unwrap();
    server.join().unwrap();
    store::save(&root, asset, bytes).unwrap();
    assert_eq!(progress[0], 0);
    assert!(progress.last().is_some_and(|percent| *percent >= 90));
    let restored = store::load_cached(&root, asset).unwrap();
    assert_eq!(restored.data.as_ref(), fixture(1));
    assert!(store::path(&root, asset).starts_with(root.join(&catalog::catalog().version)));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn truncated_and_oversized_http_responses_are_rejected() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = download::client().unwrap();
    for (body, declared) in [(vec![0; 2], 20), (vec![0; 21], 21)] {
        let (url, server) = server(body, declared);
        assert!(
            runtime
                .block_on(download::fetch_url(&client, &url, 20, &mut |_| {}))
                .is_err()
        );
        server.join().unwrap();
    }
}

#[test]
#[ignore = "downloads pinned public fonts from GitHub and Gitee; run explicitly"]
fn public_font_sources_match_pinned_content() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = download::client().unwrap();
    for asset in &catalog::catalog().fonts {
        for url in catalog::urls(asset) {
            let bytes = runtime
                .block_on(download::fetch_url(&client, &url, asset.size, &mut |_| {}))
                .unwrap_or_else(|error| panic!("{url}: {error}"));
            store::validate(asset, bytes, true).unwrap();
        }
    }
}
