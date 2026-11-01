use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

gpui_kit::assets::icon_assets!(
    AppIcons,
    [
        Gauge,
        Zap,
        Archive,
        Shield,
        CirclePause,
        Clock,
        SquareCheck,
        Circle,
        CircleX,
        ChevronDown,
        GitCompareArrows,
        GitBranch,
        FilePlus,
        FileMinus,
        FileCode
    ]
);
gpui_kit::assets::icon_assets!(ApprovalIcons, [Trash, FilePen, Terminal, Plug]);
pub struct AppAssets<T>(pub T);
impl<T: AssetSource> AssetSource for AppAssets<T> {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(data) = AppIcons.load(path)? {
            return Ok(Some(data));
        }
        if let Some(data) = ApprovalIcons.load(path)? {
            return Ok(Some(data));
        }
        self.0.load(path)
    }
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = AppIcons.list(path)?;
        names.extend(ApprovalIcons.list(path)?);
        names.extend(self.0.list(path)?);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::IconName;

    #[test]
    fn embedded_icons_resolve_through_kit() {
        let assets = AppAssets(gpui_kit::assets::Assets);
        for icon in [
            IconName::Archive,
            IconName::ArrowDown,
            IconName::ArrowUp,
            IconName::Check,
            IconName::ChevronRight,
            IconName::ChevronDown,
            IconName::Circle,
            IconName::CircleX,
            IconName::CircleAlert,
            IconName::CirclePause,
            IconName::FileCode,
            IconName::FileMinus,
            IconName::FilePlus,
            IconName::GitBranch,
            IconName::GitCompareArrows,
            IconName::Clock,
            IconName::Close,
            IconName::Copy,
            IconName::Folder,
            IconName::Info,
            IconName::PanelLeft,
            IconName::Plus,
            IconName::RefreshCw,
            IconName::Shield,
            IconName::Trash,
            IconName::FilePen,
            IconName::Terminal,
            IconName::Plug,
            IconName::Square,
            IconName::SquareCheck,
            IconName::TriangleAlert,
        ] {
            let path = icon.path();
            let bytes = assets.load(&path).unwrap().unwrap();
            assert!(
                std::str::from_utf8(&bytes).unwrap().contains("<svg"),
                "{path}"
            );
        }
    }
}
