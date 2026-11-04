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
        GitMerge,
        GitPullRequest,
        GitCommitHorizontal,
        Upload,
        FilePlus,
        FileMinus,
        FileCode,
        Hourglass,
        Layers,
        Sparkles,
        Sparkle,
        SquarePen,
        CircleArrowUp,
        WifiOff,
        FolderLock,
        FolderInput,
        ShieldAlert,
        RotateCcwClock,
        ArrowUpRight
    ]
);
gpui_kit::assets::icon_assets!(ApprovalIcons, [Trash, FilePen, Terminal, Plug]);
pub const CLAUDE_MARK: &str = "icons/claude.svg";
pub const OPENAI_MARK: &str = "icons/openai.svg";
const MARKS: [(&str, &[u8]); 2] = [
    (CLAUDE_MARK, include_bytes!("../assets/icons/claude.svg")),
    (OPENAI_MARK, include_bytes!("../assets/icons/openai.svg")),
];

pub struct AppAssets<T>(pub T);
impl<T: AssetSource> AssetSource for AppAssets<T> {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, data)) = MARKS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(data)));
        }
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
        names.extend(
            MARKS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
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
    fn provider_marks_are_served_from_the_binary() {
        let assets = AppAssets(gpui_kit::assets::Assets);
        for path in [CLAUDE_MARK, OPENAI_MARK] {
            let bytes = assets.load(path).unwrap().unwrap();
            assert!(
                std::str::from_utf8(&bytes).unwrap().contains("<path"),
                "{path}"
            );
            assert!(
                assets
                    .list("icons/")
                    .unwrap()
                    .iter()
                    .any(|name| name == path)
            );
        }
    }

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
            IconName::FolderOpen,
            IconName::Gauge,
            IconName::Hourglass,
            IconName::Info,
            IconName::Layers,
            IconName::Search,
            IconName::Sparkle,
            IconName::Sparkles,
            IconName::Zap,
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
            IconName::SquarePen,
            IconName::TriangleAlert,
            IconName::CircleArrowUp,
            IconName::WifiOff,
            IconName::FolderLock,
            IconName::FolderInput,
            IconName::ShieldAlert,
            IconName::RotateCcwClock,
            IconName::ArrowUpRight,
            IconName::Settings,
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
