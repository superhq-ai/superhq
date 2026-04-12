use gpui::*;

// ---------------------------------------------------------------------------
// Panel — a sidebar/bottom panel (review, file tree, search, etc.)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockPosition {
    Left,
    Right,
    Bottom,
}

#[derive(Clone, Debug)]
pub enum PanelEvent {
    Activate,
    Deactivate,
    ChangePosition(DockPosition),
}

pub trait Panel: EventEmitter<PanelEvent> + Render + 'static {
    fn name(&self) -> &'static str;
    fn icon(&self) -> Option<&'static str> { None }
    fn position(&self) -> DockPosition;
    fn default_size(&self) -> Pixels { px(320.0) }
    fn can_position(&self, _position: DockPosition) -> bool { true }
}

/// Type-erased handle to any Panel entity.
pub trait PanelHandle: 'static {
    fn entity_id(&self) -> EntityId;
    fn name(&self, cx: &App) -> &'static str;
    fn icon(&self, cx: &App) -> Option<&'static str>;
    fn position(&self, cx: &App) -> DockPosition;
    fn default_size(&self, cx: &App) -> Pixels;
    fn to_any_element(&self, window: &mut Window, cx: &mut App) -> AnyElement;
    fn set_active(&self, active: bool, cx: &mut App);
}

impl<T: Panel> PanelHandle for Entity<T> {
    fn entity_id(&self) -> EntityId {
        Entity::entity_id(self)
    }

    fn name(&self, cx: &App) -> &'static str {
        self.read(cx).name()
    }

    fn icon(&self, cx: &App) -> Option<&'static str> {
        self.read(cx).icon()
    }

    fn position(&self, cx: &App) -> DockPosition {
        self.read(cx).position()
    }

    fn default_size(&self, cx: &App) -> Pixels {
        self.read(cx).default_size()
    }

    fn to_any_element(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        self.update(cx, |panel, cx| panel.render(window, cx).into_any_element())
    }

    fn set_active(&self, active: bool, cx: &mut App) {
        self.update(cx, |_panel, cx| {
            if active {
                cx.emit(PanelEvent::Activate);
            } else {
                cx.emit(PanelEvent::Deactivate);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Dock — holds panels at a given position
// ---------------------------------------------------------------------------

pub struct Dock {
    position: DockPosition,
    panels: Vec<Box<dyn PanelHandle>>,
    active_panel: Option<usize>,
    size: Pixels,
    pub visible: bool,
}

impl Dock {
    pub fn new(position: DockPosition) -> Self {
        Self {
            position,
            panels: Vec::new(),
            active_panel: None,
            size: px(320.0),
            visible: false,
        }
    }

    pub fn position(&self) -> DockPosition {
        self.position
    }

    pub fn add_panel<T: Panel>(&mut self, panel: Entity<T>, cx: &mut Context<Self>) {
        cx.subscribe(&panel, |dock, _, event: &PanelEvent, cx| {
            match event {
                PanelEvent::Activate => {
                    dock.visible = true;
                    cx.notify();
                }
                PanelEvent::Deactivate => {
                    dock.visible = false;
                    cx.notify();
                }
                PanelEvent::ChangePosition(_) => {}
            }
        })
        .detach();
        if self.active_panel.is_none() {
            self.active_panel = Some(self.panels.len());
        }
        self.panels.push(Box::new(panel));
    }

    pub fn active_panel(&self) -> Option<&dyn PanelHandle> {
        self.active_panel.and_then(|i| self.panels.get(i).map(|p| p.as_ref()))
    }

    pub fn set_size(&mut self, size: Pixels) {
        self.size = size;
    }

    pub fn size(&self) -> Pixels {
        self.size
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn show(&mut self) {
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }
}

// ---------------------------------------------------------------------------
// TabItem — content inside a pane tab (agent session, shell, etc.)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum TabItemEvent {
    UpdateLabel(SharedString),
    Close,
    Ready,
}

pub trait TabItem: EventEmitter<TabItemEvent> + Render + 'static {
    fn tab_label(&self) -> SharedString;
    fn tab_icon(&self) -> Option<SharedString> { None }
    fn tab_color(&self) -> Option<Rgba> { None }
    fn can_close(&self) -> bool { true }
    fn is_dirty(&self) -> bool { false }
}

/// Type-erased handle to any TabItem entity.
pub trait TabItemHandle: 'static {
    fn entity_id(&self) -> EntityId;
    fn tab_label(&self, cx: &App) -> SharedString;
    fn tab_icon(&self, cx: &App) -> Option<SharedString>;
    fn tab_color(&self, cx: &App) -> Option<Rgba>;
    fn can_close(&self, cx: &App) -> bool;
    fn to_any_element(&self, window: &mut Window, cx: &mut App) -> AnyElement;
}

impl<T: TabItem> TabItemHandle for Entity<T> {
    fn entity_id(&self) -> EntityId {
        Entity::entity_id(self)
    }

    fn tab_label(&self, cx: &App) -> SharedString {
        self.read(cx).tab_label()
    }

    fn tab_icon(&self, cx: &App) -> Option<SharedString> {
        self.read(cx).tab_icon()
    }

    fn tab_color(&self, cx: &App) -> Option<Rgba> {
        self.read(cx).tab_color()
    }

    fn can_close(&self, cx: &App) -> bool {
        self.read(cx).can_close()
    }

    fn to_any_element(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        self.update(cx, |item, cx| item.render(window, cx).into_any_element())
    }
}

// ---------------------------------------------------------------------------
// Pane — holds tabbed items, supports future splitting
// ---------------------------------------------------------------------------

pub struct Pane {
    items: Vec<Box<dyn TabItemHandle>>,
    active_item: usize,
}

impl Pane {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            active_item: 0,
        }
    }

    pub fn add_item(&mut self, item: impl TabItemHandle) {
        self.items.push(Box::new(item));
        self.active_item = self.items.len() - 1;
    }

    pub fn remove_item(&mut self, entity_id: EntityId) {
        self.items.retain(|i| i.entity_id() != entity_id);
        if self.active_item >= self.items.len() && !self.items.is_empty() {
            self.active_item = self.items.len() - 1;
        }
    }

    pub fn activate_item(&mut self, index: usize) {
        if index < self.items.len() {
            self.active_item = index;
        }
    }

    pub fn active_item(&self) -> Option<&dyn TabItemHandle> {
        self.items.get(self.active_item).map(|i| i.as_ref())
    }

    pub fn items(&self) -> &[Box<dyn TabItemHandle>] {
        &self.items
    }

    pub fn active_index(&self) -> usize {
        self.active_item
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

// ---------------------------------------------------------------------------
// PaneLayout — recursive tree for future split views
// ---------------------------------------------------------------------------

pub enum PaneLayout {
    Single(Pane),
    Split {
        axis: Axis,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
        ratio: f32,
    },
}

impl PaneLayout {
    pub fn single() -> Self {
        Self::Single(Pane::new())
    }

    pub fn split(axis: Axis, first: PaneLayout, second: PaneLayout, ratio: f32) -> Self {
        Self::Split {
            axis,
            first: Box::new(first),
            second: Box::new(second),
            ratio,
        }
    }
}

// ---------------------------------------------------------------------------
// Dock — GPUI integration
// ---------------------------------------------------------------------------

impl Render for Dock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.active_panel() {
            Some(panel) => div()
                .id("dock-content")
                .size_full()
                .child(panel.to_any_element(window, cx)),
            None => div().id("dock-empty"),
        }
    }
}
