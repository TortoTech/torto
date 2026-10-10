use emath::Vec2;

use super::{
    BytesLoader as _, Context, HashMap, ImagePoll, Mutex, SizeHint, SizedTexture, TextureHandle,
    TextureLoadResult, TextureLoader, TextureOptions, TexturePoll,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PrimaryKey {
    uri: String,
    texture_options: TextureOptions,
}

/// SVG:s might have several different sizes loaded
type Bucket = HashMap<Option<SizeHint>, Entry>;

struct Entry {
    last_used: u64,

    /// Size of the original SVG, if any, or the texel size of the image if not an SVG.
    source_size: Vec2,

    handle: TextureHandle,
}

#[derive(Default)]
struct State {
    pass_index: u64,
    cache: HashMap<PrimaryKey, Bucket>,
}

// Torto: bound retained UI image textures, while protecting images used by the
// current/previous pass. Visible images may exceed this soft budget.
const TEXTURE_CACHE_BUDGET: usize = 64 * 1024 * 1024;

impl State {
    fn trim_to_budget(&mut self, pass_index: u64, budget: usize) {
        let mut bytes: usize = self
            .cache
            .values()
            .flat_map(|bucket| bucket.values())
            .map(|entry| entry.handle.byte_size())
            .sum();
        if bytes <= budget {
            return;
        }
        let mut unused = self
            .cache
            .iter()
            .flat_map(|(key, bucket)| {
                bucket.iter().filter_map(|(size, entry)| {
                    (pass_index.saturating_sub(entry.last_used) > 1)
                        .then(|| (entry.last_used, key.clone(), *size))
                })
            })
            .collect::<Vec<_>>();
        unused.sort_by_key(|(last_used, _, _)| *last_used);
        for (_, key, size) in unused {
            if bytes <= budget {
                break;
            }
            if let Some(bucket) = self.cache.get_mut(&key)
                && let Some(entry) = bucket.remove(&size)
            {
                bytes = bytes.saturating_sub(entry.handle.byte_size());
            }
        }
        self.cache.retain(|_, bucket| !bucket.is_empty());
    }
}

#[derive(Default)]
pub struct DefaultTextureLoader {
    state: Mutex<State>,
}

impl TextureLoader for DefaultTextureLoader {
    fn id(&self) -> &'static str {
        crate::generate_loader_id!(DefaultTextureLoader)
    }

    fn load(
        &self,
        ctx: &Context,
        uri: &str,
        texture_options: TextureOptions,
        size_hint: SizeHint,
    ) -> TextureLoadResult {
        let svg_size_hint = if is_svg(uri) {
            // For SVGs it's important that we render at the desired size,
            // or we might get a blurry image when we scale it up.
            // So we make the size hint a part of the cache key.
            // This might lead to a lot of extra entries for the same SVG file,
            // which is potentially wasteful of RAM, but better that than blurry images.
            Some(size_hint)
        } else {
            // For other images we just use one cache value, no matter what the size we render at.
            None
        };

        let mut state = self.state.lock();
        let State { pass_index, cache } = &mut *state;

        let bucket = cache
            .entry(PrimaryKey {
                uri: uri.to_owned(),
                texture_options,
            })
            .or_default();

        if let Some(texture) = bucket.get_mut(&svg_size_hint) {
            texture.last_used = *pass_index;
            let texture = SizedTexture::new(texture.handle.id(), texture.source_size);
            Ok(TexturePoll::Ready { texture })
        } else {
            match ctx.try_load_image(uri, size_hint)? {
                ImagePoll::Pending { size } => Ok(TexturePoll::Pending { size }),
                ImagePoll::Ready { image } => {
                    let source_size = image.source_size;
                    let handle = ctx.load_texture(uri, image, texture_options);
                    let texture = SizedTexture::new(handle.id(), source_size);
                    bucket.insert(
                        svg_size_hint,
                        Entry {
                            last_used: *pass_index,
                            source_size,
                            handle,
                        },
                    );
                    let reduce_texture_memory = ctx.options(|o| o.reduce_texture_memory);
                    if reduce_texture_memory {
                        let loaders = ctx.loaders();
                        loaders.include.forget(uri);
                        for loader in loaders.bytes.lock().iter().rev() {
                            loader.forget(uri);
                        }
                        for loader in loaders.image.lock().iter().rev() {
                            loader.forget(uri);
                        }
                    }
                    Ok(TexturePoll::Ready { texture })
                }
            }
        }
    }

    fn forget(&self, uri: &str) {
        log::trace!("forget {uri:?}");

        self.state.lock().cache.retain(|key, _value| key.uri != uri);
    }

    fn forget_all(&self) {
        log::trace!("forget all");

        self.state.lock().cache.clear();
    }

    fn end_pass(&self, pass_index: u64) {
        let mut state = self.state.lock();
        state.pass_index = pass_index;

        let State {
            pass_index: cached_pass_index,
            cache,
        } = &mut *state;

        cache.retain(|_key, bucket| {
            if 2 <= bucket.len() {
                // There are multiple textures of the same URI (e.g. SVGs of different scales).
                // This could be because someone has an SVG in a resizable container,
                // and so we get a lot of different sizes of it.
                // This could wast VRAM, so we remove the ones that are not used in this frame.
                bucket.retain(|_, texture| *cached_pass_index <= texture.last_used + 1);
            }
            !bucket.is_empty()
        });
        state.trim_to_budget(pass_index, TEXTURE_CACHE_BUDGET);
    }

    fn byte_size(&self) -> usize {
        self.state
            .lock()
            .cache
            .values()
            .map(|bucket| {
                bucket
                    .values()
                    .map(|texture| texture.handle.byte_size())
                    .sum::<usize>()
            })
            .sum()
    }
}

fn is_svg(uri: &str) -> bool {
    super::has_extension(uri, "svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_budget_evicts_oldest_unused_and_preserves_visible_images() {
        let ctx = Context::default();
        let mut state = State::default();
        for (uri, last_used) in [("old", 1), ("recent", 8), ("visible", 10)] {
            let handle = ctx.load_texture(
                uri,
                crate::ColorImage::new([2, 2], vec![crate::Color32::WHITE; 4]),
                TextureOptions::default(),
            );
            state.cache.insert(
                PrimaryKey {
                    uri: uri.into(),
                    texture_options: TextureOptions::default(),
                },
                [(
                    None,
                    Entry {
                        last_used,
                        source_size: Vec2::splat(2.0),
                        handle,
                    },
                )]
                .into_iter()
                .collect(),
            );
        }
        state.trim_to_budget(10, 32);
        assert_eq!(state.cache.len(), 2);
        assert!(state.cache.keys().all(|key| key.uri != "old"));
        state.trim_to_budget(10, 0);
        assert_eq!(state.cache.len(), 1);
        assert_eq!(state.cache.keys().next().unwrap().uri, "visible");
        state.trim_to_budget(12, 0);
        assert!(state.cache.is_empty());
    }
}
