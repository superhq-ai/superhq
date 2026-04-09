use gpui::{AssetSource, Result, SharedString};
use rust_embed::RustEmbed;
use std::borrow::Cow;
use std::collections::HashSet;

#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
#[include = "app-icon-128.png"]
struct AppAssets;

/// Combined asset source: our local assets first, then gpui-component defaults.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        // Try our local assets first
        if let Some(f) = AppAssets::get(path) {
            return Ok(Some(f.data));
        }
        // Fall back to gpui-component assets
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut seen = HashSet::new();
        let mut paths: Vec<SharedString> = AppAssets::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| { let s: SharedString = p.into(); seen.insert(s.clone()); s })
            .collect();
        if let Ok(component_paths) = gpui_component_assets::Assets.list(path) {
            for p in component_paths {
                if seen.insert(p.clone()) {
                    paths.push(p);
                }
            }
        }
        Ok(paths)
    }
}
