use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    sync::{Arc, Mutex},
};

use gpui_kit::{Image, ImageFormat, RenderImage, SvgRenderer};
use image_webp::WebPDecoder;
use tokio::task::{JoinSet, spawn_blocking};

use super::Sidebar;
use crate::api::SpkClient;

const MAX_IN_FLIGHT: usize = 4;
const MAX_DIMENSION: u32 = 256;
const MAX_FRAMES: u32 = 256;

pub struct AvatarPhoto {
    pub image: Arc<RenderImage>,
    pub thumbnail_sizes: Mutex<HashSet<u32>>,
}

#[derive(Default)]
pub(super) struct AvatarLoads {
    pub(super) tasks: JoinSet<Loaded>,
    attempts: HashMap<String, Attempt>,
}

struct Attempt {
    key: String,
    failed: bool,
}

pub(super) struct Loaded {
    pub(super) user_id: String,
    pub(super) key: String,
    pub(super) photo: Option<Arc<AvatarPhoto>>,
}

impl AvatarLoads {
    pub(super) fn fill(&mut self, client: &SpkClient, sidebar: &Sidebar) {
        let candidates = sidebar
            .profile
            .id
            .iter()
            .chain(sidebar.voice_presence.keys())
            .chain(sidebar.users.values().filter_map(|user| {
                (sidebar.profile.id.is_some() && user.name().is_some()).then_some(&user.id)
            }));

        for user_id in candidates {
            if self.tasks.len() >= MAX_IN_FLIGHT {
                break;
            }
            let Some(key) = sidebar.avatar_key(user_id) else {
                continue;
            };
            if self.attempts.contains_key(user_id) {
                continue;
            }
            self.attempts.insert(
                user_id.clone(),
                Attempt {
                    key: key.to_owned(),
                    failed: false,
                },
            );
            let (client, user_id, key) = (client.clone(), user_id.clone(), key.to_owned());
            self.tasks.spawn(async move {
                let photo = match client.avatar(&user_id, &key).await {
                    Ok(bytes) => spawn_blocking(move || decode(bytes)).await.ok().flatten(),
                    Err(_) => None,
                };
                Loaded {
                    user_id,
                    key,
                    photo,
                }
            });
        }
    }

    pub(super) fn directory_changed(&mut self, sidebar: &mut Sidebar) {
        self.attempts.retain(|id, attempt| {
            !attempt.failed && sidebar.avatar_key(id) == Some(attempt.key.as_str())
        });
        if sidebar
            .avatars
            .keys()
            .any(|id| !self.attempts.contains_key(id))
        {
            Arc::make_mut(&mut sidebar.avatars).retain(|id, _| self.attempts.contains_key(id));
        }
    }

    pub(super) fn complete(&mut self, sidebar: &mut Sidebar, loaded: Loaded) {
        let Loaded {
            user_id,
            key,
            photo,
        } = loaded;
        if sidebar.avatar_key(&user_id) == Some(key.as_str())
            && let Some(attempt) = self.attempts.get_mut(&user_id)
            && attempt.key == key
        {
            attempt.failed = photo.is_none();
            if let Some(photo) = photo {
                Arc::make_mut(&mut sidebar.avatars).insert(user_id, photo);
            }
        }
    }
}

fn decode(bytes: Vec<u8>) -> Option<Arc<AvatarPhoto>> {
    let header = WebPDecoder::new(Cursor::new(&bytes)).ok()?;
    let (width, height) = header.dimensions();
    if width > MAX_DIMENSION || height > MAX_DIMENSION || header.num_frames() > MAX_FRAMES {
        return None;
    }
    let image = Image::from_bytes(ImageFormat::Webp, bytes)
        .to_image_data(SvgRenderer::new(Arc::new(())))
        .ok()?;
    Some(Arc::new(AvatarPhoto {
        image,
        thumbnail_sizes: Mutex::default(),
    }))
}
