use std::{borrow::Cow, collections::BTreeMap, fmt::Write, io::Read, sync::LazyLock};

use flate2::read::GzDecoder;
use gpui_kit::{AssetSource, Result, SharedString, assets::AllAssets};

const PREFIX: &str = "twemoji/";
static ARTWORK: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    let bundle = include_bytes!("../assets/emoji/twemoji-1.2.5.json.gz");
    let mut json = Vec::new();
    GzDecoder::new(bundle.as_slice())
        .read_to_end(&mut json)
        .expect("bundled Twemoji artwork must decompress");
    serde_json::from_slice(&json).expect("bundled Twemoji artwork must be valid")
});

pub(crate) struct DesktopAssets;

impl AssetSource for DesktopAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with(PREFIX) {
            return Ok(ARTWORK.get(path).map(|svg| Cow::Borrowed(svg.as_bytes())));
        }
        AllAssets.load(path)
    }

    fn list(&self, prefix: &str) -> Result<Vec<SharedString>> {
        let mut paths = AllAssets.list(prefix)?;
        if PREFIX.starts_with(prefix) || prefix.starts_with(PREFIX) {
            paths.extend(
                ARTWORK
                    .keys()
                    .filter(|path| path.starts_with(prefix))
                    .map(|path| SharedString::from(path.as_str())),
            );
        }
        Ok(paths)
    }
}

pub(crate) fn reaction_artwork_path(emoji: &str) -> Option<String> {
    // SPK Web discards emoji presentation selectors, even inside joined emoji.
    let mut path = PREFIX.to_owned();
    for scalar in emoji.chars().filter(|&scalar| scalar != '\u{fe0f}') {
        if path.len() > PREFIX.len() {
            path.push('-');
        }
        write!(path, "{:04x}", u32::from(scalar)).expect("writing to a String cannot fail");
    }
    path.push_str(".svg");
    ARTWORK.contains_key(&path).then_some(path)
}
