use std::collections::HashMap;

use opal_gfx::{App, Bind, ImageHandle, Scene};

const RASTER_PX: u32 = 64;

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub enum Icon {
    Menu,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Settings,
    Bell,
    Play,
    Pause,
    SkipBack,
    SkipForward,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    Minimize,
    Maximize,
    Close,
    Home,
    Search,
    Plus,
    Heart,
    HeartFilled,
    Check,
    Queue,
    Devices,
    PanelRight,
}

impl Icon {
    fn svg_bytes(self) -> &'static [u8] {
        match self {
            Icon::Menu => include_bytes!("../../assets/icons/menu.svg"),
            Icon::ChevronLeft => include_bytes!("../../assets/icons/chevron-left.svg"),
            Icon::ChevronRight => include_bytes!("../../assets/icons/chevron-right.svg"),
            Icon::ChevronDown => include_bytes!("../../assets/icons/chevron-down.svg"),
            Icon::Settings => include_bytes!("../../assets/icons/settings.svg"),
            Icon::Bell => include_bytes!("../../assets/icons/bell.svg"),
            Icon::Play => include_bytes!("../../assets/icons/play.svg"),
            Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
            Icon::SkipBack => include_bytes!("../../assets/icons/skip-back.svg"),
            Icon::SkipForward => include_bytes!("../../assets/icons/skip-forward.svg"),
            Icon::Shuffle => include_bytes!("../../assets/icons/shuffle.svg"),
            Icon::Repeat => include_bytes!("../../assets/icons/repeat.svg"),
            Icon::RepeatOne => include_bytes!("../../assets/icons/repeat-1.svg"),
            Icon::Volume => include_bytes!("../../assets/icons/volume.svg"),
            Icon::Minimize => include_bytes!("../../assets/icons/minimize.svg"),
            Icon::Maximize => include_bytes!("../../assets/icons/maximize.svg"),
            Icon::Close => include_bytes!("../../assets/icons/close.svg"),
            Icon::Home => include_bytes!("../../assets/icons/home.svg"),
            Icon::Search => include_bytes!("../../assets/icons/search.svg"),
            Icon::Plus => include_bytes!("../../assets/icons/plus.svg"),
            Icon::Heart => include_bytes!("../../assets/icons/heart.svg"),
            Icon::HeartFilled => include_bytes!("../../assets/icons/heart-filled.svg"),
            Icon::Check => include_bytes!("../../assets/icons/check.svg"),
            Icon::Queue => include_bytes!("../../assets/icons/queue.svg"),
            Icon::Devices => include_bytes!("../../assets/icons/devices.svg"),
            Icon::PanelRight => include_bytes!("../../assets/icons/panel-right.svg"),
        }
    }
}

const ALL: &[Icon] = &[
    Icon::Menu,
    Icon::ChevronLeft,
    Icon::ChevronRight,
    Icon::ChevronDown,
    Icon::Settings,
    Icon::Bell,
    Icon::Play,
    Icon::Pause,
    Icon::SkipBack,
    Icon::SkipForward,
    Icon::Shuffle,
    Icon::Repeat,
    Icon::RepeatOne,
    Icon::Volume,
    Icon::Minimize,
    Icon::Maximize,
    Icon::Close,
    Icon::Home,
    Icon::Search,
    Icon::Plus,
    Icon::Heart,
    Icon::HeartFilled,
    Icon::Check,
    Icon::Queue,
    Icon::Devices,
    Icon::PanelRight,
];

/// Raster size for the brand logo (gradient dragonfly). Larger than the
/// monochrome icons since it renders bigger (beside the app name) and must
/// keep its gradient crisp.
const LOGO_PX: u32 = 192;

#[derive(Clone)]
pub struct IconSet {
    handles: HashMap<Icon, ImageHandle>,
    /// The full-colour Opal brand mark (gradient dragonfly). Rendered
    /// untinted via [`IconSet::render_logo`] — unlike the monochrome icons,
    /// it must NOT be colour-tinted.
    logo: ImageHandle,
}

impl IconSet {
    pub fn get(&self, icon: Icon) -> ImageHandle {
        *self.handles.get(&icon).expect("icon not loaded — extend ALL")
    }

    /// The brand logo handle (gradient-preserving — draw it untinted with
    /// `OPAQUE_TINT`, not an accent tint).
    pub fn logo(&self) -> ImageHandle {
        self.logo
    }

    pub fn render(
        &self,
        s: &mut Scene,
        icon: Icon,
        size_px: f32,
        color: impl Into<Bind<[f32; 4]>>,
    ) {
        s.image((), self.get(icon))
            .w_px(size_px)
            .h_px(size_px)
            .color(color);
    }

    /// Render the brand logo at `size_px` (square), keeping its gradient —
    /// no colour tint. Used in the login/setup header beside "Opal".
    pub fn render_logo(&self, s: &mut Scene, size_px: f32) {
        s.image((), self.logo).w_px(size_px).h_px(size_px);
    }
}

pub fn load_all<S>(app: &mut App<S>) -> IconSet {
    let mut handles = HashMap::with_capacity(ALL.len());
    for &icon in ALL {
        let h = app.stage_image_svg(icon.svg_bytes(), RASTER_PX);
        handles.insert(icon, h);
    }
    let logo = app.stage_image_svg(include_bytes!("../../assets/logo/geometric-opal.svg"), LOGO_PX);
    IconSet { handles, logo }
}
