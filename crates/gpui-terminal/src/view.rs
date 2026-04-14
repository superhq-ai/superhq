//! Main terminal view component for GPUI.
//!
//! This module provides [`TerminalView`], the primary component for embedding terminals
//! in GPUI applications. It manages:
//!
//! - **I/O Streams**: Accepts arbitrary [`Read`]/[`Write`]
//!   streams, allowing integration with any PTY implementation
//! - **Event Handling**: Keyboard and mouse input, with configurable callbacks
//! - **Rendering**: Efficient canvas-based rendering via [`TerminalRenderer`]
//! - **Configuration**: Font, colors, dimensions, and padding via [`TerminalConfig`]
//!
//! # Architecture
//!
//! The terminal uses a push-based async I/O architecture:
//!
//! 1. A background thread reads bytes from the PTY stdout in 4KB chunks
//! 2. Bytes are sent through a [flume](https://docs.rs/flume) channel to an async task
//! 3. The async task processes bytes through the VTE parser and calls `cx.notify()`
//! 4. GPUI repaints the terminal with the updated grid
//!
//! This approach ensures the terminal only wakes when data arrives, avoiding polling.
//!
//! # Thread Safety
//!
//! - [`TerminalView`] itself is not `Send` (it contains GPUI handles)
//! - The stdin writer is wrapped in `Arc<parking_lot::Mutex<>>` for thread-safe writes
//! - Callbacks ([`ResizeCallback`], [`KeyHandler`]) must be `Send + Sync`
//!
//! # Example
//!
//! ```ignore
//! use gpui::{Context, Edges, px};
//! use gpui_terminal::{ColorPalette, TerminalConfig, TerminalView};
//!
//! // In a GPUI window context:
//! let terminal = cx.new(|cx| {
//!     TerminalView::new(pty_writer, pty_reader, TerminalConfig::default(), cx)
//!         .with_resize_callback(move |cols, rows| {
//!             // Notify PTY of new dimensions
//!         })
//!         .with_exit_callback(|_, cx| {
//!             cx.quit();
//!         })
//! });
//!
//! // Focus the terminal to receive keyboard input
//! terminal.read(cx).focus_handle().focus(window);
//! ```

use crate::colors::ColorPalette;
use crate::event::{GpuiEventProxy, TerminalEvent};
use crate::input::keystroke_to_bytes;
use crate::render::TerminalRenderer;
use crate::terminal::{InternalEvent, SelectionPhase, TerminalState};
use alacritty_terminal::index::{Column as AlacColumn, Line as AlacLine, Point as AlacPoint};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::index::Side as AlacSide;
use gpui::{Edges, *};
use std::io::{Read, Write};

// Actions for context menu and keybindings
actions!(terminal, [TermCopy, TermPaste, TermClear, TermSelectAll]);
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

// Scrollbar constants (matching gpui_component style)
const SCROLLBAR_TRACK_WIDTH: f32 = 14.0;
const SB_THUMB_WIDTH: f32 = 6.0;
const SB_THUMB_ACTIVE_WIDTH: f32 = 8.0;
const SB_THUMB_INSET: f32 = 4.0;
const SB_THUMB_RADIUS: f32 = 3.0;
const SB_THUMB_ACTIVE_RADIUS: f32 = 4.0;
const SB_MIN_THUMB_HEIGHT: f32 = 48.0;
const SB_FADE_OUT_DELAY: f32 = 2.0;
const SB_FADE_OUT_DURATION: f32 = 3.0;

#[derive(Clone, Copy)]
struct ScrollbarInner {
    dragging: bool,
    drag_start_y: f32,
    hovered: bool,
    hovered_thumb: bool,
    last_scroll_time: Option<Instant>,
}

impl Default for ScrollbarInner {
    fn default() -> Self {
        Self {
            dragging: false,
            drag_start_y: 0.0,
            hovered: false,
            hovered_thumb: false,
            last_scroll_time: None,
        }
    }
}

#[derive(Clone)]
struct TermScrollbar(Rc<Cell<ScrollbarInner>>);

impl TermScrollbar {
    fn new() -> Self {
        Self(Rc::new(Cell::new(ScrollbarInner::default())))
    }
    fn get(&self) -> ScrollbarInner { self.0.get() }
    fn set(&self, v: ScrollbarInner) { self.0.set(v); }
}

/// Configuration for terminal creation and runtime updates.
///
/// This struct defines the terminal's appearance and behavior, including
/// grid dimensions, font settings, scrollback buffer, and color scheme.
///
/// # Default Values
///
/// | Field | Default |
/// |-------|---------|
/// | `cols` | 80 |
/// | `rows` | 24 |
/// | `font_family` | "monospace" |
/// | `font_size` | 14px |
/// | `scrollback` | 10000 |
/// | `line_height_multiplier` | 1.0 |
/// | `padding` | 0px all sides |
/// | `colors` | Default palette |
///
/// # Example
///
/// ```ignore
/// use gpui::{Edges, px};
/// use gpui_terminal::{ColorPalette, TerminalConfig};
///
/// let config = TerminalConfig {
///     cols: 120,
///     rows: 40,
///     font_family: "JetBrains Mono".into(),
///     font_size: px(13.0),
///     scrollback: 50000,
///     line_height_multiplier: 1.0,
///     padding: Edges::all(px(10.0)),
///     colors: ColorPalette::builder()
///         .background(0x1a, 0x1a, 0x1a)
///         .foreground(0xe0, 0xe0, 0xe0)
///         .build(),
/// };
/// ```
///
/// # Runtime Updates
///
/// Configuration can be updated at runtime via [`TerminalView::update_config`].
/// This is useful for implementing features like dynamic font sizing:
///
/// ```ignore
/// terminal.update(cx, |terminal, cx| {
///     let mut config = terminal.config().clone();
///     config.font_size += px(1.0);
///     terminal.update_config(config, cx);
/// });
/// ```
#[derive(Clone, Debug)]
pub struct TerminalConfig {
    /// Number of columns (character width) in the terminal
    pub cols: usize,

    /// Number of rows (lines) in the terminal
    pub rows: usize,

    /// Font family name (e.g., "Fira Code", "JetBrains Mono")
    pub font_family: String,

    /// Font size in pixels
    pub font_size: Pixels,

    /// Maximum number of scrollback lines to keep in history
    pub scrollback: usize,

    /// Multiplier for line height to accommodate tall glyphs (e.g., nerd fonts)
    /// Default is 1.0 (no extra height)
    pub line_height_multiplier: f32,

    /// Padding around the terminal content (top, right, bottom, left)
    /// The padding area renders with the terminal's background color
    pub padding: Edges<Pixels>,

    /// Color palette for terminal colors (16 ANSI colors, 256 extended colors,
    /// foreground, background, and cursor colors)
    pub colors: ColorPalette,

    /// Scrollbar thumb color at its "peak" alpha (dragging/hovered state).
    /// Hover and resting states derive from this by scaling alpha.
    pub scrollbar_thumb: Hsla,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            font_family: "monospace".into(),
            font_size: px(14.0),
            scrollback: 10000,
            line_height_multiplier: 1.0,
            padding: Edges::all(px(0.0)),
            colors: ColorPalette::default(),
            scrollbar_thumb: gpui::hsla(0.0, 0.0, 1.0, 0.5),
        }
    }
}

/// Callback type for PTY resize notifications.
///
/// This callback is invoked when the terminal grid dimensions change,
/// typically due to window resizing. The callback receives the new
/// column and row counts.
///
/// # Arguments
///
/// * `cols` - New number of columns (characters wide)
/// * `rows` - New number of rows (lines tall)
///
/// # Thread Safety
///
/// This callback must be `Send + Sync` as it may be called from the render thread.
///
/// # Example
///
/// ```ignore
/// use portable_pty::PtySize;
///
/// let pty = Arc::new(Mutex::new(pty_master));
/// let pty_clone = pty.clone();
///
/// terminal.with_resize_callback(move |cols, rows| {
///     pty_clone.lock().resize(PtySize {
///         cols: cols as u16,
///         rows: rows as u16,
///         pixel_width: 0,
///         pixel_height: 0,
///     }).ok();
/// });
/// ```
pub type ResizeCallback = Box<dyn Fn(usize, usize) + Send + Sync>;

/// Callback type for key event interception.
///
/// This callback is invoked before the terminal processes a key event,
/// allowing you to intercept and handle specific key combinations.
///
/// # Arguments
///
/// * `event` - The key down event from GPUI
///
/// # Returns
///
/// * `true` - Consume the event (terminal will not process it)
/// * `false` - Let the terminal handle the event normally
///
/// # Thread Safety
///
/// This callback must be `Send + Sync`.
///
/// # Example
///
/// ```ignore
/// terminal.with_key_handler(|event| {
///     let keystroke = &event.keystroke;
///
///     // Intercept Ctrl++ for font size increase
///     if keystroke.modifiers.control && (keystroke.key == "+" || keystroke.key == "=") {
///         // Handle font size increase
///         return true; // Consume the event
///     }
///
///     // Intercept Ctrl+- for font size decrease
///     if keystroke.modifiers.control && keystroke.key == "-" {
///         // Handle font size decrease
///         return true;
///     }
///
///     false // Let terminal handle all other keys
/// });
/// ```
pub type KeyHandler = Box<dyn Fn(&KeyDownEvent) -> bool + Send + Sync>;

/// Callback for terminal bell events.
///
/// This callback is invoked when the terminal bell is triggered (BEL character,
/// ASCII 0x07), allowing you to play a sound or show a visual indicator.
///
/// # Arguments
///
/// * `window` - The GPUI window
/// * `cx` - The context for the TerminalView
///
/// # Example
///
/// ```ignore
/// terminal.with_bell_callback(|window, cx| {
///     // Option 1: Visual bell (flash the window or show an indicator)
///     // Option 2: Play a sound
///     // Option 3: Notify the user via system notification
/// });
/// ```
pub type BellCallback = Box<dyn Fn(&mut Window, &mut Context<TerminalView>)>;

/// Callback for terminal title changes.
///
/// This callback is invoked when the terminal title changes via escape sequences
/// (OSC 0, OSC 2), allowing you to update the window or tab title.
///
/// # Arguments
///
/// * `window` - The GPUI window
/// * `cx` - The context for the TerminalView
/// * `title` - The new title string
///
/// # Example
///
/// ```ignore
/// terminal.with_title_callback(|window, cx, title| {
///     // Update the window title
///     // Or update a tab label in a tabbed interface
///     println!("Terminal title changed to: {}", title);
/// });
/// ```
pub type TitleCallback = Box<dyn Fn(&mut Window, &mut Context<TerminalView>, &str)>;

/// Callback for clipboard store requests.
///
/// This callback is invoked when the terminal wants to store data to the clipboard
/// via OSC 52 escape sequence. Applications like tmux and vim can use this to
/// copy text to the system clipboard.
///
/// # Arguments
///
/// * `window` - The GPUI window
/// * `cx` - The context for the TerminalView
/// * `text` - The text to store in the clipboard
///
/// # Example
///
/// ```ignore
/// use gpui_terminal::Clipboard;
///
/// terminal.with_clipboard_store_callback(|window, cx, text| {
///     if let Ok(mut clipboard) = Clipboard::new() {
///         clipboard.copy(text).ok();
///     }
/// });
/// ```
pub type ClipboardStoreCallback = Box<dyn Fn(&mut Window, &mut Context<TerminalView>, &str)>;

/// Callback for terminal exit events.
///
/// This callback is invoked when the terminal process exits (e.g., shell exits,
/// process terminates). This is detected when the PTY reader reaches EOF.
///
/// # Arguments
///
/// * `window` - The GPUI window
/// * `cx` - The context for the TerminalView
///
/// # Example
///
/// ```ignore
/// terminal.with_exit_callback(|window, cx| {
///     // Option 1: Quit the application
///     cx.quit();
///
///     // Option 2: Close this terminal tab/pane
///     // terminal_manager.close_terminal(terminal_id);
///
///     // Option 3: Show an exit message
///     // show_notification("Terminal exited");
/// });
/// ```
pub type ExitCallback = Box<dyn Fn(&mut Window, &mut Context<TerminalView>)>;

/// The main terminal view component for GPUI applications.
///
/// `TerminalView` is a GPUI entity that implements the [`Render`] trait,
/// providing a complete terminal emulator that can be embedded in any GPUI application.
///
/// # Responsibilities
///
/// - **Terminal State**: Manages the grid, cursor, and colors via [`TerminalState`]
/// - **I/O Streams**: Reads from PTY stdout and writes to PTY stdin
/// - **Event Handling**: Processes keyboard, mouse, and resize events
/// - **Rendering**: Paints text, backgrounds, and cursor via [`TerminalRenderer`]
/// - **Callbacks**: Dispatches events to user-provided callbacks
///
/// # Creating a Terminal
///
/// Use [`TerminalView::new`] within a GPUI entity context:
///
/// ```ignore
/// let terminal = cx.new(|cx| {
///     TerminalView::new(writer, reader, config, cx)
///         .with_resize_callback(resize_callback)
///         .with_exit_callback(|_, cx| cx.quit())
/// });
/// ```
///
/// # Focus
///
/// The terminal must be focused to receive keyboard input:
///
/// ```ignore
/// terminal.read(cx).focus_handle().focus(window);
/// ```
///
/// # Callbacks
///
/// Configure behavior through builder methods:
///
/// - [`with_resize_callback`](Self::with_resize_callback) - PTY size changes
/// - [`with_exit_callback`](Self::with_exit_callback) - Process exit
/// - [`with_key_handler`](Self::with_key_handler) - Key event interception
/// - [`with_bell_callback`](Self::with_bell_callback) - Terminal bell
/// - [`with_title_callback`](Self::with_title_callback) - Title changes
/// - [`with_clipboard_store_callback`](Self::with_clipboard_store_callback) - Clipboard writes
///
/// # Thread Safety
///
/// `TerminalView` is not `Send` as it contains GPUI handles. The stdin writer
/// is internally wrapped in `Arc<parking_lot::Mutex<>>` for safe concurrent access.
pub struct TerminalView {
    /// The terminal state managing the grid and VTE parser
    state: TerminalState,

    /// The renderer for drawing terminal content
    renderer: TerminalRenderer,

    /// Last known canvas bounds (updated every paint, used for mouse→grid conversion)
    last_bounds: Arc<parking_lot::Mutex<Bounds<Pixels>>>,

    /// Focus handle for keyboard event handling
    focus_handle: FocusHandle,

    /// Writer for sending input to the terminal process
    stdin_writer: Arc<parking_lot::Mutex<Box<dyn Write + Send>>>,

    /// Receiver for terminal events from the event proxy
    event_rx: mpsc::Receiver<TerminalEvent>,

    /// Configuration used to create this terminal
    config: TerminalConfig,

    /// Async task that reads bytes and notifies the view (push-based)
    #[allow(dead_code)]
    _reader_task: Task<()>,

    /// Callback to notify the PTY about size changes
    resize_callback: Option<Arc<ResizeCallback>>,

    /// Optional callback to intercept key events before terminal processing
    key_handler: Option<Arc<KeyHandler>>,

    /// Callback for terminal bell events
    bell_callback: Option<BellCallback>,

    /// Callback for terminal title changes
    title_callback: Option<TitleCallback>,

    /// Callback for clipboard store requests
    clipboard_store_callback: Option<ClipboardStoreCallback>,

    /// Callback for terminal exit events
    exit_callback: Option<ExitCallback>,

    /// Whether we've sent the initial focus-in event to the PTY
    sent_focus_in: bool,

    /// Detected URLs from last paint (shared with mouse handlers)
    url_hits: std::rc::Rc<std::cell::RefCell<Vec<crate::render::UrlHit>>>,
    /// Currently hovered URL (shared with renderer for highlight)
    hovered_url: std::rc::Rc<std::cell::RefCell<Option<crate::render::UrlHit>>>,

    /// Scrollbar state for visual feedback and drag interaction
    scrollbar: TermScrollbar,
}

impl TerminalView {
    /// Create a new terminal with provided I/O streams.
    ///
    /// This method initializes a new terminal emulator with the given stdin writer
    /// and stdout reader. It spawns a background task to read from stdout and
    /// process incoming bytes through the VTE parser.
    ///
    /// # Arguments
    ///
    /// * `stdin_writer` - Writer for sending input bytes to the terminal process
    /// * `stdout_reader` - Reader for receiving output bytes from the terminal process
    /// * `config` - Terminal configuration (dimensions, font, etc.)
    /// * `cx` - GPUI context for this view
    ///
    /// # Returns
    ///
    /// A new `TerminalView` instance ready to be rendered.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // In a GPUI window context:
    /// let terminal = cx.new(|cx| {
    ///     TerminalView::new(stdin_writer, stdout_reader, TerminalConfig::default(), cx)
    /// });
    /// ```
    pub fn new<W, R>(
        stdin_writer: W,
        stdout_reader: R,
        config: TerminalConfig,
        cx: &mut Context<Self>,
    ) -> Self
    where
        W: Write + Send + 'static,
        R: Read + Send + 'static,
    {
        // Create event channel for terminal events
        let (event_tx, event_rx) = mpsc::channel();

        // Clone event_tx for the reader task to send Exit event when PTY closes
        let exit_event_tx = event_tx.clone();

        // Create event proxy for alacritty
        let event_proxy = GpuiEventProxy::new(event_tx);

        // Create terminal state
        let state = TerminalState::new(config.cols, config.rows, config.scrollback, event_proxy);

        // Create renderer with font settings and color palette
        let renderer = TerminalRenderer::new(
            config.font_family.clone(),
            config.font_size,
            config.line_height_multiplier,
            config.colors.clone(),
        );

        // Create focus handle
        let focus_handle = cx.focus_handle();

        // Wrap stdin writer in Arc<Mutex> for thread-safe access
        let stdin_writer = Arc::new(parking_lot::Mutex::new(
            Box::new(stdin_writer) as Box<dyn Write + Send>
        ));

        // Create async channel for bytes (push-based notification)
        // Using flume instead of smol::channel because flume is executor-agnostic
        // and properly wakes GPUI's async executor when data arrives
        let (bytes_tx, bytes_rx) = flume::unbounded::<Vec<u8>>();

        // Spawn background thread to read from stdout
        // This thread sends bytes through the async channel
        thread::spawn(move || {
            Self::read_stdout_blocking(stdout_reader, bytes_tx);
        });

        // Spawn async task that awaits on the channel and notifies the view
        // This is push-based: the task blocks until bytes arrive, then immediately notifies
        let reader_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                // Wait for bytes from the background reader (blocks until data arrives)
                match bytes_rx.recv_async().await {
                    Ok(bytes) => {
                        let result = this.update(cx, |view: &mut Self, cx: &mut Context<Self>| {
                            view.state.process_bytes(&bytes);
                            cx.notify();
                        });
                        if result.is_err() {
                            // View was dropped, exit
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = exit_event_tx.send(TerminalEvent::Exit);
                        let _ = this.update(cx, |_view, cx: &mut Context<Self>| {
                            cx.notify();
                        });
                        break;
                    }
                }
            }
        });

        Self {
            state,
            renderer,
            last_bounds: Arc::new(parking_lot::Mutex::new(Bounds::default())),
            focus_handle,
            stdin_writer,
            event_rx,
            config,
            _reader_task: reader_task,
            resize_callback: None,
            key_handler: None,
            bell_callback: None,
            title_callback: None,
            clipboard_store_callback: None,
            exit_callback: None,
            sent_focus_in: false,
            url_hits: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            hovered_url: std::rc::Rc::new(std::cell::RefCell::new(None)),
            scrollbar: TermScrollbar::new(),
        }
    }

    /// Set a callback to be invoked when the terminal is resized.
    ///
    /// This callback should resize the underlying PTY to match the new dimensions.
    /// The callback receives (cols, rows) as arguments.
    ///
    /// # Arguments
    ///
    /// * `callback` - A function that will be called with (cols, rows) on resize
    pub fn with_resize_callback(
        mut self,
        callback: impl Fn(usize, usize) + Send + Sync + 'static,
    ) -> Self {
        self.resize_callback = Some(Arc::new(Box::new(callback)));
        self
    }

    /// Set a callback to intercept key events before terminal processing.
    ///
    /// The callback receives the key event and should return `true` to consume
    /// the event (prevent the terminal from processing it), or `false` to allow
    /// normal terminal processing.
    ///
    /// # Arguments
    ///
    /// * `handler` - A function that receives key events and returns whether to consume them
    ///
    /// # Example
    ///
    /// ```ignore
    /// terminal.with_key_handler(|event| {
    ///     // Handle Ctrl++ to increase font size
    ///     if event.keystroke.modifiers.control && event.keystroke.key == "+" {
    ///         // Handle the event
    ///         return true; // Consume the event
    ///     }
    ///     false // Let terminal handle it
    /// })
    /// ```
    pub fn with_key_handler(
        mut self,
        handler: impl Fn(&KeyDownEvent) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.key_handler = Some(Arc::new(Box::new(handler)));
        self
    }

    /// Set a callback to be invoked when the terminal bell is triggered.
    ///
    /// The callback receives a mutable reference to the window and context,
    /// allowing you to play a sound or show a visual indicator.
    ///
    /// # Arguments
    ///
    /// * `callback` - A function that will be called when the bell is triggered
    ///
    /// # Example
    ///
    /// ```ignore
    /// terminal.with_bell_callback(|window, cx| {
    ///     // Play a sound or flash the screen
    /// })
    /// ```
    pub fn with_bell_callback(
        mut self,
        callback: impl Fn(&mut Window, &mut Context<TerminalView>) + 'static,
    ) -> Self {
        self.bell_callback = Some(Box::new(callback));
        self
    }

    /// Set a callback to be invoked when the terminal title changes.
    ///
    /// The callback receives a mutable reference to the window and context,
    /// along with the new title string.
    ///
    /// # Arguments
    ///
    /// * `callback` - A function that will be called with the new title
    ///
    /// # Example
    ///
    /// ```ignore
    /// terminal.with_title_callback(|window, cx, title| {
    ///     // Update window title or tab title
    /// })
    /// ```
    pub fn with_title_callback(
        mut self,
        callback: impl Fn(&mut Window, &mut Context<TerminalView>, &str) + 'static,
    ) -> Self {
        self.title_callback = Some(Box::new(callback));
        self
    }

    /// Set a callback to be invoked when the terminal wants to store data to the clipboard.
    ///
    /// The callback receives a mutable reference to the window and context,
    /// along with the text to store. This is typically triggered by OSC 52 escape sequences.
    ///
    /// # Arguments
    ///
    /// * `callback` - A function that will be called with the text to store
    ///
    /// # Example
    ///
    /// ```ignore
    /// terminal.with_clipboard_store_callback(|window, cx, text| {
    ///     // Store text to system clipboard
    /// })
    /// ```
    pub fn with_clipboard_store_callback(
        mut self,
        callback: impl Fn(&mut Window, &mut Context<TerminalView>, &str) + 'static,
    ) -> Self {
        self.clipboard_store_callback = Some(Box::new(callback));
        self
    }

    /// Set a callback to be invoked when the terminal process exits.
    ///
    /// The callback receives a mutable reference to the window and context,
    /// allowing you to close the terminal view or show an exit message.
    ///
    /// # Arguments
    ///
    /// * `callback` - A function that will be called when the process exits
    ///
    /// # Example
    ///
    /// ```ignore
    /// terminal.with_exit_callback(|window, cx| {
    ///     // Close the terminal tab or show exit message
    /// })
    /// ```
    pub fn with_exit_callback(
        mut self,
        callback: impl Fn(&mut Window, &mut Context<TerminalView>) + 'static,
    ) -> Self {
        self.exit_callback = Some(Box::new(callback));
        self
    }

    /// Background thread that reads from stdout.
    ///
    /// This function runs in a background thread, continuously reading bytes
    /// from the stdout reader and sending them through the async channel.
    /// The async channel allows the main async task to be woken up immediately
    /// when data arrives (push-based).
    fn read_stdout_blocking<R: Read + Send + 'static>(
        mut stdout_reader: R,
        bytes_tx: flume::Sender<Vec<u8>>,
    ) {
        let mut buffer = [0u8; 4096];

        loop {
            match stdout_reader.read(&mut buffer) {
                Ok(0) => {
                    break;
                }
                Ok(n) => {
                    let bytes = buffer[..n].to_vec();
                    if bytes_tx.send(bytes).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    break;
                }
            }
        }
    }

    /// Handle keyboard input events.
    ///
    /// Write bytes to the PTY stdin.
    fn write_to_pty(&self, bytes: &[u8]) {
        let mut writer = self.stdin_writer.lock();
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }

    /// Copy current selection to clipboard (trimming trailing whitespace per line).
    fn copy_selection(&self, cx: &App) {
        // Read selection text fresh from the term (not from the cached snapshot,
        // which may be stale between sync cycles).
        let text = self.state.with_term(|term| {
            term.selection.as_ref().and_then(|sel| {
                let range = sel.to_range(term)?;
                Some(term.bounds_to_string(range.start, range.end))
            })
        });
        if let Some(text) = text {
            if !text.is_empty() {
                let trimmed: String = text.lines()
                    .map(|l| l.trim_end())
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(ClipboardItem::new_string(trimmed));
            }
        }
    }

    /// Paste from clipboard into the PTY with bracketed paste support.
    fn paste_clipboard(&self, cx: &App) {
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(text) = item.text() {
                if !text.is_empty() {
                    let bracketed = self.state.content.mode
                        .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
                    if bracketed {
                        // Filter \x1b and \x03 to prevent premature paste
                        // termination in some shells.
                        let filtered = text.replace(['\x1b', '\x03'], "");
                        self.write_to_pty(b"\x1b[200~");
                        self.write_to_pty(filtered.as_bytes());
                        self.write_to_pty(b"\x1b[201~");
                    } else {
                        // Without bracketed paste, convert \n → \r
                        // (simulating Enter keypresses).
                        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
                        self.write_to_pty(normalized.as_bytes());
                    }
                }
            }
        }
    }

    /// Send focus-in event to the PTY if the app requested FOCUS_IN_OUT mode.
    fn ensure_focus_in_sent(&mut self) {
        if !self.sent_focus_in
            && self.state.mode().contains(alacritty_terminal::term::TermMode::FOCUS_IN_OUT)
        {
            self.write_to_pty(b"\x1b[I");
            self.sent_focus_in = true;
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_focus_in_sent();

        // Check if key handler wants to consume this event
        if let Some(ref handler) = self.key_handler
            && handler(event)
        {
            return;
        }

        // Handle Cmd/platform shortcuts
        if event.keystroke.modifiers.platform {
            match event.keystroke.key.as_str() {
                "v" => { self.paste_clipboard(cx); return; }
                "c" => { self.copy_selection(cx); return; }
                _ => {}
            }
        }

        if let Some(bytes) = keystroke_to_bytes(&event.keystroke, self.state.mode()) {
            self.write_to_pty(&bytes);
        }
    }

    /// Handle mouse down events.
    ///
    /// Currently a placeholder for future mouse selection and interaction support.
    /// Convert pixel position to grid point (column, line).
    fn pixel_to_grid(&self, position: Point<Pixels>) -> AlacPoint {
        use alacritty_terminal::grid::Dimensions;

        let bounds = *self.last_bounds.lock();
        let padding = &self.config.padding;
        let cell_w: f32 = self.renderer.cell_width.into();
        let cell_h: f32 = self.renderer.cell_height.into();

        if cell_w <= 0.0 || cell_h <= 0.0 {
            return AlacPoint::new(AlacLine(0), AlacColumn(0));
        }

        let x: f32 = (position.x - bounds.origin.x - padding.left).into();
        let y: f32 = (position.y - bounds.origin.y - padding.top).into();

        let col = (x / cell_w).max(0.0) as usize;
        let line = (y / cell_h).max(0.0) as usize;

        // Read actual dimensions and scroll offset from the term
        let (max_col, max_line, display_offset) = self.state.with_term(|term| {
            (
                term.columns().saturating_sub(1),
                term.screen_lines().saturating_sub(1),
                term.grid().display_offset(),
            )
        });

        let visual_line = line.min(max_line);
        // Convert visual viewport row to grid line:
        // When scrolled, viewport row 0 maps to grid Line(-display_offset)
        let grid_line = visual_line as i32 - display_offset as i32;

        AlacPoint::new(
            AlacLine(grid_line),
            AlacColumn(col.min(max_col)),
        )
    }

    /// Encode modifier keys for mouse reports (delegates to mouse module).
    fn mouse_modifier_bits(event_mods: &gpui::Modifiers) -> u8 {
        crate::mouse::encode_modifiers(event_mods.shift, event_mods.alt, event_mods.control)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.ensure_focus_in_sent();

        // Cmd+click on URL → open in browser
        if event.button == MouseButton::Left && event.modifiers.platform {
            let point = self.pixel_to_grid(event.position);
            let hits = self.url_hits.borrow();
            if let Some(hit) = hits.iter().find(|h| {
                let grid_line = point.line.0 + self.state.with_term(|t| t.grid().display_offset() as i32);
                h.line_idx as i32 == grid_line
                    && point.column.0 >= h.start_col
                    && point.column.0 < h.end_col
            }) {
                let url = hit.url.clone();
                drop(hits);
                let _ = open::that(&url);
                return;
            }
        }

        let point = self.pixel_to_grid(event.position);

        // Mouse reporting mode
        if self.state.mouse_mode(event.modifiers.shift) {
            let mods = Self::mouse_modifier_bits(&event.modifiers);
            if let Some(bytes) = crate::mouse::mouse_button_report(
                event.button, true, point, mods, self.state.mode(),
            ) {
                self.write_to_pty(&bytes);
            }
            return;
        }

        // Selection mode
        let sel_type = match event.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };

        if event.click_count == 1 {
            self.state.push_event(InternalEvent::SetSelection(None));
        }

        let selection = Selection::new(sel_type, point, AlacSide::Left);
        self.state.push_event(InternalEvent::SetSelection(Some((selection, point))));
        self.state.selection_phase = SelectionPhase::Selecting;

        cx.notify();
    }

    fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Mouse reporting
        if self.state.mouse_mode(event.modifiers.shift) {
            let point = self.pixel_to_grid(event.position);
            let mods = Self::mouse_modifier_bits(&event.modifiers);
            if let Some(bytes) = crate::mouse::mouse_button_report(
                event.button, false, point, mods, self.state.mode(),
            ) {
                self.write_to_pty(&bytes);
            }
            return;
        }

        if self.state.selection_phase == SelectionPhase::Selecting {
            self.state.selection_phase = SelectionPhase::Selected;
            cx.notify();
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Mouse reporting (motion)
        if self.state.mouse_mode(event.modifiers.shift) && event.pressed_button.is_some() {
            let point = self.pixel_to_grid(event.position);
            let mods = Self::mouse_modifier_bits(&event.modifiers);
            // Motion report: button 32 + actual button
            let motion_code = 32u8 | mods; // Left button motion
            let col = point.column.0 + 1;
            let row = point.line.0 + 1;
            let seq = format!("\x1b[<{};{};{}M", motion_code, col, row);
            self.write_to_pty(seq.as_bytes());
            return;
        }

        if self.state.selection_phase != SelectionPhase::Selecting {
            return;
        }

        let point = self.pixel_to_grid(event.position);
        self.state.push_event(InternalEvent::UpdateSelection(point));
        cx.notify();
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cell_h: f32 = self.renderer.cell_height.into();
        if cell_h <= 0.0 {
            return;
        }

        let line_height = px(cell_h);
        let delta_y: f32 = event.delta.pixel_delta(line_height).y.into();

        // Mouse reporting: send scroll events to PTY
        if self.state.mouse_mode(event.modifiers.shift) {
            let point = self.pixel_to_grid(event.position);
            let mods = Self::mouse_modifier_bits(&event.modifiers);
            // Positive delta = scroll up in content = mouse button 64
            // Negative delta = scroll down in content = mouse button 65
            let scroll_delta = if delta_y > 0.0 { -1 } else { 1 };
            if let Some(bytes) = crate::mouse::scroll_report(
                scroll_delta, point, mods, self.state.mode(),
            ) {
                self.write_to_pty(&bytes);
            }
            return;
        }

        // Alt screen + ALTERNATE_SCROLL: convert scroll to arrow keys
        if self.state.alt_screen()
            && self.state.mode().contains(alacritty_terminal::term::TermMode::ALTERNATE_SCROLL)
        {
            let lines = (delta_y / cell_h).round() as i32;
            if lines != 0 {
                // Positive delta = content moves down = arrow up keys
                let key = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
                for _ in 0..lines.unsigned_abs() {
                    self.write_to_pty(key);
                }
            }
            return;
        }

        // Normal scrollback — direct, no easing
        let old_offset = (self.state.scroll_px / cell_h) as i32;
        self.state.scroll_px += delta_y;
        let new_offset = (self.state.scroll_px / cell_h) as i32;

        // Prevent unbounded growth
        let bounds_h: f32 = self.last_bounds.lock().size.height.into();
        if bounds_h > 0.0 {
            self.state.scroll_px %= bounds_h;
        }

        let lines = new_offset - old_offset;
        if lines != 0 {
            self.state.scroll_px -= (lines as f32) * cell_h;
            self.state.push_event(InternalEvent::Scroll(
                alacritty_terminal::grid::Scroll::Delta(lines),
            ));
            let mut sb = self.scrollbar.get();
            sb.last_scroll_time = Some(Instant::now());
            self.scrollbar.set(sb);
            cx.notify();
        }
    }

    /// Process pending terminal events.
    ///
    /// This method drains all available events from the event receiver
    /// and handles them appropriately. Note: bytes are processed in the
    /// async reader task, not here.
    fn process_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Process terminal events (from alacritty event proxy)
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                TerminalEvent::Wakeup => {
                    // Terminal has new content - already handled by async task
                }
                TerminalEvent::Bell => {
                    if let Some(ref callback) = self.bell_callback {
                        callback(window, cx);
                    }
                }
                TerminalEvent::Title(title) => {
                    if let Some(ref callback) = self.title_callback {
                        callback(window, cx, &title);
                    }
                }
                TerminalEvent::ClipboardStore(text) => {
                    if let Some(ref callback) = self.clipboard_store_callback {
                        callback(window, cx, &text);
                    }
                }
                TerminalEvent::ClipboardLoad(format) => {
                    // Terminal app is requesting clipboard contents (OSC 52 read).
                    // Read from system clipboard, format as OSC 52 response, write to PTY.
                    if let Some(item) = cx.read_from_clipboard() {
                        if let Some(text) = item.text() {
                            let response = format(&text);
                            self.write_to_pty(response.as_bytes());
                        }
                    }
                }
                TerminalEvent::PtyWrite(data) => {
                    // Write terminal response back to PTY (cursor position,
                    // device attributes, etc.). TUI apps block on these.
                    self.write_to_pty(data.as_bytes());
                }
                TerminalEvent::Exit => {
                    if let Some(ref callback) = self.exit_callback {
                        callback(window, cx);
                    }
                }
            }
        }
    }

    /// Get the current terminal dimensions.
    ///
    /// # Returns
    ///
    /// A tuple of (columns, rows).
    pub fn dimensions(&self) -> (usize, usize) {
        (self.state.cols(), self.state.rows())
    }

    /// Resize the terminal to new dimensions.
    ///
    /// This method should be called when the terminal view size changes.
    /// It updates the internal grid and notifies the terminal process of the new size.
    ///
    /// # Arguments
    ///
    /// * `cols` - New number of columns
    /// * `rows` - New number of rows
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.state.resize(cols, rows);
    }

    /// Get the current terminal configuration.
    ///
    /// # Returns
    ///
    /// A reference to the current configuration.
    pub fn config(&self) -> &TerminalConfig {
        &self.config
    }

    /// Get the focus handle for this terminal view.
    ///
    /// # Returns
    ///
    /// A reference to the focus handle.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Returns true if a TUI has taken over the terminal.
    /// Reads directly from the terminal emulator (not the cached render snapshot)
    /// so this works even when the view isn't being rendered yet.
    ///
    /// Detection signals (any one triggers):
    /// - ALT_SCREEN: traditional fullscreen TUIs (vim, less)
    /// - Cursor hidden (!SHOW_CURSOR): some TUI frameworks hide the terminal cursor
    /// - FOCUS_IN_OUT: TUI frameworks (Ink, Ratatui) enable focus tracking;
    ///   plain bash does not
    pub fn tui_active(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        let mode = self.state.mode();
        mode.contains(TermMode::ALT_SCREEN)
            || !mode.contains(TermMode::SHOW_CURSOR)
            || mode.contains(TermMode::FOCUS_IN_OUT)
    }


    /// Update the terminal configuration.
    ///
    /// This method updates the terminal's configuration, including font settings,
    /// padding, and color palette. Changes take effect on the next render.
    ///
    /// # Arguments
    ///
    /// * `config` - The new configuration to apply
    /// * `cx` - The context for triggering a repaint
    pub fn update_config(&mut self, config: TerminalConfig, cx: &mut Context<Self>) {
        // Update renderer with new font settings and palette
        self.renderer.font_family = config.font_family.clone();
        self.renderer.font_size = config.font_size;
        self.renderer.line_height_multiplier = config.line_height_multiplier;
        self.renderer.palette = config.colors.clone();

        // Store the new config
        self.config = config;

        // Trigger a repaint - cell dimensions will be recalculated via measure_cell()
        cx.notify();
    }

    /// Calculate terminal dimensions from pixel bounds and cell size.
    ///
    /// Helper method to determine how many columns and rows fit in the given bounds.
    #[allow(dead_code)]
    fn calculate_dimensions(&self, bounds: Bounds<Pixels>) -> (usize, usize) {
        let width_f32: f32 = bounds.size.width.into();
        let height_f32: f32 = bounds.size.height.into();
        let cell_width_f32: f32 = self.renderer.cell_width.into();
        let cell_height_f32: f32 = self.renderer.cell_height.into();

        let cols = ((width_f32 / cell_width_f32) as usize).max(1);
        let rows = ((height_f32 / cell_height_f32) as usize).max(1);
        (cols, rows)
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Process any pending events
        self.process_events(window, cx);

        // Sync: drain event queue, rebuild content snapshot
        self.state.sync();

        // Measure cell dimensions from the actual font so that pixel_to_grid
        // (called between frames on mouse events) uses accurate values.
        self.renderer.measure_cell(window);

        // Handle resize in the sync phase (before paint)
        let state_arc = self.state.term_arc();
        let renderer = self.renderer.clone();
        let resize_callback = self.resize_callback.clone();
        let padding = self.config.padding;
        let scrollbar_thumb = self.config.scrollbar_thumb;

        // Capture data for the paint closure
        let selection_range = self.state.content.selection_range.clone();
        let cursor_shape = self.state.content.cursor.shape;
        let is_focused = self.focus_handle.is_focused(window);
        let bounds_storage = self.last_bounds.clone();
        let scrollbar = self.scrollbar.clone();
        let url_hits = self.url_hits.clone();
        let hovered_url = self.hovered_url.clone();

        div()
            .size_full()
            .relative()
            .bg(rgb(0x1e1e1e))
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_action(cx.listener(|this, _: &TermCopy, _window, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &TermPaste, _window, cx| {
                this.paste_clipboard(cx);
            }))
            .on_action(cx.listener(|this, _: &TermClear, _window, _cx| {
                this.write_to_pty(b"\x0c");
            }))
            .child({
                let bounds_for_prepaint = bounds_storage.clone();
                let entity_for_drag = cx.entity().downgrade();
                let entity_for_scrollbar = entity_for_drag.clone();
                let scrollbar = scrollbar.clone();
                canvas(
                    move |bounds, _window, _cx| {
                        *bounds_for_prepaint.lock() = bounds;
                        bounds
                    },
                    move |bounds, _, window, cx| {
                        // Register window-level mouse move handler for drag-outside-bounds
                        // (Zed pattern: window.on_mouse_event fires for ALL mouse events)
                        let entity = entity_for_drag.clone();
                        window.on_mouse_event::<MouseMoveEvent>(move |event, phase, _win, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            if event.pressed_button.is_none() {
                                return;
                            }
                            entity.update(cx, |view: &mut TerminalView, cx| {
                                if view.state.selection_phase != SelectionPhase::Selecting {
                                    return;
                                }

                                let point = view.pixel_to_grid(event.position);
                                view.state.push_event(InternalEvent::UpdateSelection(point));

                                // Auto-scroll when dragging above/below terminal bounds
                                let bounds = *view.last_bounds.lock();
                                let top: f32 = bounds.origin.y.into();
                                let bottom: f32 = (bounds.origin.y + bounds.size.height).into();
                                let mouse_y: f32 = event.position.y.into();

                                if mouse_y < top {
                                    // Above: scroll up (clamped to 3 lines max)
                                    let dist = top - mouse_y;
                                    let cell_h: f32 = view.renderer.cell_height.into();
                                    let lines = ((dist / cell_h).ceil() as i32).min(3).max(1);
                                    view.state.push_event(InternalEvent::Scroll(
                                        alacritty_terminal::grid::Scroll::Delta(lines),
                                    ));
                                } else if mouse_y > bottom {
                                    // Below: scroll down
                                    let dist = mouse_y - bottom;
                                    let cell_h: f32 = view.renderer.cell_height.into();
                                    let lines = ((dist / cell_h).ceil() as i32).min(3).max(1);
                                    view.state.push_event(InternalEvent::Scroll(
                                        alacritty_terminal::grid::Scroll::Delta(-lines),
                                    ));
                                }

                                cx.notify();
                            }).ok();
                        });
                        use alacritty_terminal::grid::Dimensions;

                        // renderer clone already has measured cell dimensions from render()
                        let measured_renderer = renderer.clone();

                        // Calculate available space after padding
                        let available_width: f32 =
                            (bounds.size.width - padding.left - padding.right).into();
                        let available_height: f32 =
                            (bounds.size.height - padding.top - padding.bottom).into();
                        let cell_width_f32: f32 = measured_renderer.cell_width.into();
                        let cell_height_f32: f32 = measured_renderer.cell_height.into();

                        let cols = ((available_width / cell_width_f32) as usize).max(1);
                        let rows = ((available_height / cell_height_f32) as usize).max(1);

                        // Resize terminal if dimensions changed
                        {
                            struct TermSize { cols: usize, rows: usize }
                            impl Dimensions for TermSize {
                                fn total_lines(&self) -> usize { self.rows }
                                fn screen_lines(&self) -> usize { self.rows }
                                fn columns(&self) -> usize { self.cols }
                                fn last_column(&self) -> alacritty_terminal::index::Column {
                                    alacritty_terminal::index::Column(self.cols.saturating_sub(1))
                                }
                                fn bottommost_line(&self) -> alacritty_terminal::index::Line {
                                    alacritty_terminal::index::Line(self.rows as i32 - 1)
                                }
                                fn topmost_line(&self) -> alacritty_terminal::index::Line {
                                    alacritty_terminal::index::Line(0)
                                }
                            }

                            let mut term = state_arc.lock();
                            let current_cols = term.columns();
                            let current_rows = term.screen_lines();
                            if cols != current_cols || rows != current_rows {
                                if let Some(ref callback) = resize_callback {
                                    callback(cols, rows);
                                }
                                term.resize(TermSize { cols, rows });
                            }
                        }

                        // Paint from the term lock with selection + cursor shape
                        let term = state_arc.lock();
                        measured_renderer.paint(
                            bounds, padding, &term, &selection_range,
                            cursor_shape, is_focused,
                            Some(&hovered_url), Some(&url_hits),
                            window, cx,
                        );

                        // Set cursor to pointing hand when hovering a URL
                        if hovered_url.borrow().is_some() {
                            window.set_window_cursor_style(gpui::CursorStyle::PointingHand);
                        }

                        // Paint scrollbar with interaction support
                        {
                            use alacritty_terminal::grid::Dimensions as _;
                            let grid = term.grid();
                            let history = grid.history_size();
                            let screen = grid.screen_lines();
                            let display_offset = grid.display_offset();

                            if history > 0 {
                                let sb = scrollbar.get();
                                let track_h: f32 = bounds.size.height.into();

                                let thumb_h = (screen as f32 / (history + screen) as f32 * track_h)
                                    .max(SB_MIN_THUMB_HEIGHT)
                                    .min(track_h);

                                let scroll_ratio = display_offset as f32 / history as f32;
                                let thumb_top = (1.0 - scroll_ratio) * (track_h - thumb_h);

                                let is_active = sb.dragging || sb.hovered_thumb;
                                let tw = if is_active { SB_THUMB_ACTIVE_WIDTH } else { SB_THUMB_WIDTH };

                                let track_bounds = Bounds::new(
                                    point(
                                        bounds.origin.x + bounds.size.width - px(SCROLLBAR_TRACK_WIDTH),
                                        bounds.origin.y,
                                    ),
                                    size(px(SCROLLBAR_TRACK_WIDTH), bounds.size.height),
                                );

                                let thumb_bounds = Bounds::new(
                                    point(
                                        bounds.origin.x + bounds.size.width - px(SB_THUMB_INSET + tw),
                                        bounds.origin.y + px(thumb_top),
                                    ),
                                    size(px(tw), px(thumb_h)),
                                );

                                // Paint thumb with state-dependent styling
                                let is_visible = sb.dragging || sb.hovered || sb_is_visible(&scrollbar);
                                if is_visible {
                                    let base = scrollbar_thumb;
                                    let thumb = |factor: f32| gpui::Hsla { a: base.a * factor, ..base };
                                    let (thumb_color, radius) = if sb.dragging || sb.hovered_thumb {
                                        (thumb(1.0), px(SB_THUMB_ACTIVE_RADIUS))
                                    } else if sb.hovered {
                                        (thumb(0.8), px(SB_THUMB_ACTIVE_RADIUS))
                                    } else {
                                        (thumb(0.7 * sb_opacity(&scrollbar)), px(SB_THUMB_RADIUS))
                                    };

                                    window.paint_quad(
                                        fill(thumb_bounds, thumb_color).corner_radii(radius),
                                    );
                                }

                                // Scrollbar mouse interaction handlers
                                let scrollbar = scrollbar.clone();
                                let entity = entity_for_scrollbar.clone();
                                let track_h_val = track_h;
                                let thumb_h_val = thumb_h;
                                let history_f = history as f32;

                                // MouseDown: start drag on thumb, or jump on track click
                                window.on_mouse_event({
                                    let scrollbar = scrollbar.clone();
                                    let entity = entity.clone();
                                    move |event: &MouseDownEvent, phase, window, cx| {
                                        if !phase.bubble() { return; }
                                        if !track_bounds.contains(&event.position) { return; }
                                        cx.stop_propagation();

                                        let mut s = scrollbar.get();
                                        s.last_scroll_time = Some(Instant::now());

                                        if thumb_bounds.contains(&event.position) {
                                            s.dragging = true;
                                            s.drag_start_y = f32::from(event.position.y - thumb_bounds.origin.y);
                                        } else {
                                            // Click on track — jump to position
                                            let percentage = ((f32::from(event.position.y) - f32::from(track_bounds.origin.y) - thumb_h_val / 2.0)
                                                / (track_h_val - thumb_h_val))
                                                .clamp(0.0, 1.0);
                                            let target_offset = ((1.0 - percentage) * history_f).round() as usize;

                                            entity.update(cx, |view: &mut TerminalView, cx| {
                                                view.state.with_term_mut(|term| {
                                                    use alacritty_terminal::grid::Dimensions as _;
                                                    let current = term.grid().display_offset();
                                                    let h = term.grid().history_size();
                                                    let target = target_offset.min(h);
                                                    let delta = target as i32 - current as i32;
                                                    if delta != 0 {
                                                        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(delta));
                                                    }
                                                });
                                                cx.notify();
                                            }).ok();
                                        }
                                        scrollbar.set(s);
                                        window.refresh();
                                    }
                                });

                                // MouseMove: hover states + drag scrolling
                                window.on_mouse_event({
                                    let scrollbar = scrollbar.clone();
                                    let entity = entity.clone();
                                    move |event: &MouseMoveEvent, _phase, window, cx| {
                                        let mut s = scrollbar.get();
                                        let mut changed = false;

                                        let was_hovered = s.hovered;
                                        s.hovered = track_bounds.contains(&event.position);
                                        if s.hovered != was_hovered {
                                            if s.hovered { s.last_scroll_time = Some(Instant::now()); }
                                            changed = true;
                                        }

                                        let was_thumb = s.hovered_thumb;
                                        s.hovered_thumb = thumb_bounds.contains(&event.position);
                                        if s.hovered_thumb != was_thumb { changed = true; }

                                        if s.dragging && event.dragging() {
                                            let percentage = ((f32::from(event.position.y) - s.drag_start_y - f32::from(track_bounds.origin.y))
                                                / (track_h_val - thumb_h_val))
                                                .clamp(0.0, 1.0);
                                            let target_offset = ((1.0 - percentage) * history_f).round() as usize;

                                            entity.update(cx, |view: &mut TerminalView, cx| {
                                                view.state.with_term_mut(|term| {
                                                    use alacritty_terminal::grid::Dimensions as _;
                                                    let current = term.grid().display_offset();
                                                    let h = term.grid().history_size();
                                                    let target = target_offset.min(h);
                                                    let delta = target as i32 - current as i32;
                                                    if delta != 0 {
                                                        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(delta));
                                                    }
                                                });
                                                cx.notify();
                                            }).ok();

                                            s.last_scroll_time = Some(Instant::now());
                                            changed = true;
                                        }

                                        if changed {
                                            scrollbar.set(s);
                                            window.refresh();
                                        }
                                    }
                                });

                                // MouseUp: end drag
                                window.on_mouse_event({
                                    let scrollbar = scrollbar.clone();
                                    move |_event: &MouseUpEvent, phase, window, _cx| {
                                        if !phase.bubble() { return; }
                                        let mut s = scrollbar.get();
                                        if s.dragging {
                                            s.dragging = false;
                                            scrollbar.set(s);
                                            window.refresh();
                                        }
                                    }
                                });
                            }
                        }

                        // URL hover detection
                        {
                            let url_hits = url_hits.clone();
                            let hovered_url = hovered_url.clone();
                            let cell_w: f32 = measured_renderer.cell_width.into();
                            let cell_h: f32 = measured_renderer.cell_height.into();
                            let origin = gpui::point(
                                bounds.origin.x + padding.left,
                                bounds.origin.y + padding.top,
                            );

                            window.on_mouse_event(move |event: &MouseMoveEvent, _phase, window, _cx| {
                                let x: f32 = (event.position.x - origin.x).into();
                                let y: f32 = (event.position.y - origin.y).into();
                                if x < 0.0 || y < 0.0 { return; }
                                let col = (x / cell_w) as usize;
                                let line_idx = (y / cell_h) as usize;

                                let hits = url_hits.borrow();
                                let found = hits.iter().find(|h| {
                                    h.line_idx == line_idx
                                        && col >= h.start_col
                                        && col < h.end_col
                                });

                                let mut current = hovered_url.borrow_mut();
                                let was_some = current.is_some();
                                if let Some(hit) = found {
                                    if !was_some || current.as_ref().map(|c| &c.url) != Some(&hit.url) {
                                        *current = Some(hit.clone());
                                        window.refresh();
                                    }
                                } else if was_some {
                                    *current = None;
                                    window.refresh();
                                }
                            });
                        }
                    },
                )
                .size_full()
            })
    }
}

fn sb_is_visible(scrollbar: &TermScrollbar) -> bool {
    let s = scrollbar.get();
    if s.dragging || s.hovered { return true; }
    match s.last_scroll_time {
        None => false,
        Some(t) => Instant::now().duration_since(t).as_secs_f32() < SB_FADE_OUT_DURATION,
    }
}

fn sb_opacity(scrollbar: &TermScrollbar) -> f32 {
    let s = scrollbar.get();
    if s.dragging || s.hovered { return 1.0; }
    match s.last_scroll_time {
        None => 0.0,
        Some(t) => {
            let elapsed = Instant::now().duration_since(t).as_secs_f32();
            if elapsed < SB_FADE_OUT_DELAY {
                1.0
            } else if elapsed < SB_FADE_OUT_DURATION {
                1.0 - ((elapsed - SB_FADE_OUT_DELAY) / (SB_FADE_OUT_DURATION - SB_FADE_OUT_DELAY))
            } else {
                0.0
            }
        }
    }
}
