//! Native Windows controls, presentation and asynchronous desktop lifecycle.
//! All HWND/GDI access stays on the message-pump thread through windows-rs.
//! Device discovery and model/audio execution report bounded messages back here.
use super::{Apartment, OwnedChild, os_error, wide};
use crate::{
    Error, Result,
    controls::{
        DESKTOP_EVENT_CAPACITY, DesktopOptions, MAX_CHILD_EVENT_BYTES, MAX_PICKER_RESPONSE_BYTES,
        METER_STALE_AFTER, Preferences, ProcessingControl, UI_REFRESH_INTERVAL,
    },
    gpu::AdapterInfo,
    i18n::{Language, Text},
};
use ::windows::{
    Win32::{
        Foundation::{
            COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT,
            RECT, WPARAM,
        },
        Graphics::Gdi::{
            CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_BTNFACE, CreateFontW, DEFAULT_CHARSET,
            DEFAULT_GUI_FONT, DEFAULT_PITCH, DeleteObject, FW_NORMAL, GetStockObject,
            GetSysColorBrush, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, RDW_ALLCHILDREN, RDW_ERASE,
            RDW_INVALIDATE, RedrawWindow,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::*, HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::EnableWindow,
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

const SOURCE: u16 = 101;
const OUTPUT: u16 = 102;
const REFRESH: u16 = 103;
const STRENGTH: u16 = 104;
const PERCENT: u16 = 105;
const METER: u16 = 106;
const START: u16 = 108;
const SETTINGS: u16 = 109;
const LANGUAGE: u16 = 110;
const STATUS: u16 = 111;
const ROUTE_HINT: u16 = 112;
const SUBTITLE: u16 = 121;
const SOURCE_LABEL: u16 = 130;
const OUTPUT_LABEL: u16 = 131;
const STRENGTH_LABEL: u16 = 132;
const SIGNAL_LABEL: u16 = 133;
const BUNDLE: u16 = 201;
const BROWSE: u16 = 202;
const ADAPTER: u16 = 203;
const SAVE: u16 = 204;
const CANCEL_PICKER: u16 = 205;
const INSTALLER: u16 = 206;
const MODEL_LABEL: u16 = 210;
const ADAPTER_LABEL: u16 = 211;
const DELAY: u16 = 212;
const ECHO: u16 = 213;
const ECHO_PERCENT: u16 = 214;
const ECHO_LABEL: u16 = 215;
const ECHO_TIP: u16 = 216;
const CLASS: PCWSTR = w!("Clearwave.Native.Window");
const BASE_DPI: i32 = 96;
const POINTS_PER_INCH: i32 = 72;
const BODY_FONT_POINTS: i32 = 11;
const FLAG_WIDTH: i32 = 24;
const FLAG_HEIGHT: i32 = 18;
const FLAG_FIRST_RESOURCE: usize = 301;
const APP_ICON_RESOURCE: usize = 1;
const MAIN_WIDTH: i32 = 680;
const MAIN_HEIGHT: i32 = 690;
const SETTINGS_WIDTH: i32 = 640;
const SETTINGS_HEIGHT: i32 = 460;
const MARGIN: i32 = 24;
const GAP: i32 = 8;
const CONTROL_HEIGHT: i32 = 32;
const LINE_HEIGHT: i32 = 24;
const DROPDOWN_HEIGHT: i32 = 280;
const LANGUAGE_WIDTH: i32 = 230;
const ACTION_WIDTH: i32 = 140;
const SETTINGS_ACTION_WIDTH: i32 = 180;
const REFRESH_WIDTH: i32 = 92;
const BROWSE_WIDTH: i32 = 116;
const PERCENT_WIDTH: i32 = 60;
const METER_HEIGHT: i32 = 10;
const METER_STEPS: usize = 1000;
const METER_FLOOR_DBFS: f64 = -60.0;
const LOG_FLOOR: f64 = 1e-6;
const PERCENT_MAX: usize = 100;
const MAX_PATH_TEXT: usize = 4096;
const TIMER_ID: usize = 1;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const FONT_BODY: usize = 0;
const FONT_SETTINGS: usize = 1;
const FLAG_TRANSPARENT_COLOR: COLORREF = COLORREF(0x00ff00ff);

enum Event {
    Devices(Result<super::audio::Endpoints>),
    Adapters(Result<Vec<AdapterInfo>>),
    Worker(u32, serde_json::Value),
    Log(u32, String),
}
struct Live {
    process: OwnedChild,
    control: PathBuf,
    id: u32,
}
impl Drop for Live {
    fn drop(&mut self) {
        self.process.stop(Some(self.control.clone()));
    }
}
struct App {
    windows: OwnedWindows,
    controls: BTreeMap<u16, HWND>,
    fonts: Vec<HFONT>,
    flags: OwnedImages,
    preferences: Preferences,
    path: PathBuf,
    can_save: bool,
    inputs: Vec<super::audio::Endpoint>,
    outputs: Vec<super::audio::Endpoint>,
    adapters: Vec<String>,
    devices_task: Option<super::task::JsonTask>,
    adapters_task: Option<super::task::JsonTask>,
    sender: SyncSender<Event>,
    receiver: Receiver<Event>,
    live: Option<Live>,
    picker: Option<OwnedChild>,
    running: bool,
    levels_at: Option<Instant>,
    message: String,
}
thread_local! { static APP: RefCell<Option<App>> = const { RefCell::new(None) }; }

#[derive(Default)]
struct OwnedWindows {
    main: HWND,
    settings: HWND,
}
impl OwnedWindows {
    fn create() -> Result<Self> {
        let mut windows = Self {
            main: window("Clearwave", None)?,
            ..Self::default()
        };
        windows.settings = window(Language::default().text(Text::Settings), Some(windows.main))?;
        Ok(windows)
    }
    fn destroy(&mut self) {
        // SAFETY: these are this thread's two exclusively owned top-level windows.
        // Clear each handle before destruction, because native callbacks may re-enter.
        // Destroy the owned settings window before its owner; children follow parents.
        unsafe {
            for handle in [&mut self.settings, &mut self.main] {
                let window = std::mem::take(handle);
                if !window.0.is_null() {
                    let _ = DestroyWindow(window);
                }
            }
        }
    }
}
impl Drop for OwnedWindows {
    fn drop(&mut self) {
        self.destroy();
    }
}
#[derive(Default)]
struct OwnedImages(HIMAGELIST);
impl Drop for OwnedImages {
    fn drop(&mut self) {
        // SAFETY: this guard owns an image list created by ImageList_Create.
        // A caller detaches or replaces the list before dropping this guard.
        // App destroys all consuming windows before releasing its final list.
        unsafe {
            if self.0 != HIMAGELIST::default() {
                let _ = ImageList_Destroy(Some(self.0));
            }
        }
    }
}
fn dpi(window: HWND) -> i32 {
    // SAFETY: callers pass a live window owned by the current UI thread.
    // This query returns a scalar and does not acquire or retain any handle.
    // The baseline handles unsupported DPI contexts without changing window state.
    unsafe { GetDpiForWindow(window).max(BASE_DPI as u32) as i32 }
}
fn delete_fonts(fonts: impl IntoIterator<Item = HFONT>) {
    // SAFETY: these fonts were created for, and are exclusively owned by, this UI.
    // Callers replace their controls' fonts or destroy those controls first.
    // Deleting a null handle after a failed allocation has no resource to release.
    unsafe {
        for font in fonts {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
}

fn set_text(window: HWND, text: &str) {
    let text = wide(text);

    // SAFETY: the handle belongs to this UI thread and the text buffer is owned.
    // SetWindowTextW copies the terminating UTF-16 string during this call.
    // No worker receives the window handle or retains the temporary pointer.
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(text.as_ptr()));
    }
}
fn get_text(window: HWND) -> String {
    // SAFETY: the owned control is read on its creating UI thread.
    // The explicit buffer length bounds GetWindowTextW including its terminator.
    // The returned count selects only initialized UTF-16 units before decoding.
    unsafe {
        let count = GetWindowTextLengthW(window).clamp(0, MAX_PATH_TEXT as i32) as usize;
        let mut buffer = vec![0; count + 1];
        let used = GetWindowTextW(window, &mut buffer).max(0) as usize;
        String::from_utf16_lossy(&buffer[..used])
    }
}
fn send(window: HWND, message: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: callers supply the parameter layout required by each control message.
    // Pointer arguments refer to owned buffers or structs live for this call.
    // SendMessageW runs synchronously; child controls copy rather than retain data.
    unsafe { SendMessageW(window, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}
fn selected(window: HWND) -> Option<usize> {
    usize::try_from(send(window, CB_GETCURSEL, 0, 0)).ok()
}
fn fill_combo(window: HWND, items: &[String], choice: Option<usize>) {
    send(window, CB_RESETCONTENT, 0, 0);
    for text in items {
        let text = wide(text);
        send(window, CB_ADDSTRING, 0, text.as_ptr() as isize);
    }
    send(window, CB_SETCURSEL, choice.unwrap_or(usize::MAX), 0);
}
fn child(parent: HWND, class: PCWSTR, id: u16, style: u32) -> Result<HWND> {
    // SAFETY: parent is a live window on this thread and class names are static.
    // Windows owns each child until the parent RAII guard destroys its hierarchy.
    // Integer child IDs are passed as menu identifiers, never dereferenced handles.
    unsafe {
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!(""),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
            0,
            0,
            10,
            10,
            Some(parent),
            Some(HMENU(id as usize as *mut _)),
            None,
            None,
        )
        .map_err(os_error)?;
        let _ = SetWindowTheme(window, w!("Explorer"), None);
        Ok(window)
    }
}
fn window(title: &str, parent: Option<HWND>) -> Result<HWND> {
    let title = wide(title);

    // SAFETY: the registered class procedure and title buffer are valid here.
    // The title is copied, and the returned HWND immediately enters OwnedWindows.
    // The class and native controls share Windows' dialog surface and text colors.
    unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS,
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            MAIN_WIDTH,
            MAIN_HEIGHT,
            parent,
            None,
            Some(HINSTANCE(GetModuleHandleW(None).map_err(os_error)?.0)),
            None,
        )
        .map_err(os_error)
    }
}

impl App {
    fn control(&self, id: u16) -> HWND {
        self.controls[&id]
    }
    fn language(&self) -> Language {
        self.preferences.language
    }
    fn text(&self, key: Text) -> &'static str {
        self.language().text(key)
    }
    fn create(windows: OwnedWindows, path: PathBuf, bundle: Option<PathBuf>) -> Result<Self> {
        let (main, settings) = (windows.main, windows.settings);
        let (preferences, can_save, message) = match Preferences::load(&path) {
            Ok(preferences) => (preferences, true, String::new()),
            Err(error) => (Preferences::default(), false, error.to_string()),
        };
        let (sender, receiver) = mpsc::sync_channel(DESKTOP_EVENT_CAPACITY);
        let mut app = Self {
            windows,
            path,
            preferences,
            can_save,
            message,
            controls: BTreeMap::new(),
            fonts: Vec::new(),
            flags: OwnedImages::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            adapters: Vec::new(),
            devices_task: None,
            adapters_task: None,
            sender,
            receiver,
            live: None,
            picker: None,
            running: false,
            levels_at: None,
        };
        if let Some(bundle) = bundle {
            app.preferences.active.bundle = bundle.to_string_lossy().into_owned();
        }
        for id in [
            SUBTITLE,
            SOURCE_LABEL,
            OUTPUT_LABEL,
            STRENGTH_LABEL,
            SIGNAL_LABEL,
            PERCENT,
            ECHO_LABEL,
            ECHO_PERCENT,
            STATUS,
            ROUTE_HINT,
        ] {
            app.controls.insert(id, child(main, w!("STATIC"), id, 0)?);
        }
        for id in [SOURCE, OUTPUT] {
            app.controls.insert(
                id,
                child(
                    main,
                    w!("COMBOBOX"),
                    id,
                    CBS_DROPDOWNLIST as u32 | WS_TABSTOP.0 | WS_VSCROLL.0,
                )?,
            );
        }
        for id in [REFRESH, START, SETTINGS] {
            app.controls
                .insert(id, child(main, w!("BUTTON"), id, WS_TABSTOP.0)?);
        }
        app.controls.insert(
            LANGUAGE,
            child(
                main,
                WC_COMBOBOXEXW,
                LANGUAGE,
                CBS_DROPDOWNLIST as u32 | WS_TABSTOP.0 | WS_VSCROLL.0,
            )?,
        );
        app.controls.insert(
            STRENGTH,
            child(main, TRACKBAR_CLASSW, STRENGTH, WS_TABSTOP.0 | TBS_NOTICKS)?,
        );
        app.controls
            .insert(METER, child(main, PROGRESS_CLASSW, METER, PBS_SMOOTH)?);
        for id in [MODEL_LABEL, ADAPTER_LABEL, DELAY, ECHO_TIP] {
            app.controls
                .insert(id, child(settings, w!("STATIC"), id, 0)?);
        }
        app.controls.insert(
            BUNDLE,
            child(
                settings,
                w!("EDIT"),
                BUNDLE,
                WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
            )?,
        );
        send(app.control(BUNDLE), EM_SETLIMITTEXT, MAX_PATH_TEXT, 0);
        app.controls.insert(
            ADAPTER,
            child(
                settings,
                w!("COMBOBOX"),
                ADAPTER,
                CBS_DROPDOWNLIST as u32 | WS_TABSTOP.0 | WS_VSCROLL.0,
            )?,
        );
        for id in [BROWSE, INSTALLER, SAVE, CANCEL_PICKER] {
            app.controls
                .insert(id, child(settings, w!("BUTTON"), id, WS_TABSTOP.0)?);
        }
        app.controls.insert(
            ECHO,
            child(main, TRACKBAR_CLASSW, ECHO, WS_TABSTOP.0 | TBS_NOTICKS)?,
        );
        set_text(app.control(BUNDLE), &app.preferences.active.bundle);
        for (id, value) in [
            (STRENGTH, app.preferences.active.strength),
            (ECHO, app.preferences.active.echo_reduction),
        ] {
            send(
                app.control(id),
                TBM_SETRANGE,
                1,
                (PERCENT_MAX << u16::BITS) as isize,
            );
            send(
                app.control(id),
                TBM_SETPOS,
                1,
                (value * PERCENT_MAX as f32) as isize,
            );
        }
        send(app.control(METER), PBM_SETRANGE32, 0, METER_STEPS as isize);
        app.order_tab_stops(
            main,
            &[
                LANGUAGE, SOURCE, REFRESH, OUTPUT, STRENGTH, ECHO, SETTINGS, START,
            ],
        )?;
        app.order_tab_stops(
            settings,
            &[BUNDLE, BROWSE, INSTALLER, CANCEL_PICKER, ADAPTER, SAVE],
        )?;
        app.rebuild_fonts()?;
        app.translate();
        app.layout(main);
        app.layout(settings);
        Ok(app)
    }
    fn order_tab_stops(&self, parent: HWND, order: &[u16]) -> Result<()> {
        let mut previous = HWND_TOP;

        // SAFETY: all listed controls are siblings owned by the supplied parent.
        // Native dialog traversal follows their sibling z-order and WS_TABSTOP.
        // Geometry and focus remain unchanged while establishing visual tab order.
        unsafe {
            for id in order {
                let control = self.control(*id);
                debug_assert!(IsChild(parent, control).as_bool());
                SetWindowPos(
                    control,
                    Some(previous),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
                .map_err(os_error)?;
                previous = control;
            }
        }
        Ok(())
    }
    fn rebuild_fonts(&mut self) -> Result<()> {
        let main_dpi = dpi(self.windows.main);
        let settings_dpi = dpi(self.windows.settings);

        // SAFETY: native resources stay on this UI thread and are owned by App.
        // Controls switch to valid replacements before any previous resource drops.
        // Image/UTF-16 buffers remain live throughout each synchronous copy call.
        unsafe {
            let new_fonts: Vec<_> = [
                (BODY_FONT_POINTS, FW_NORMAL, main_dpi),
                (BODY_FONT_POINTS, FW_NORMAL, settings_dpi),
            ]
            .into_iter()
            .map(|(size, weight, font_dpi)| {
                CreateFontW(
                    -size * font_dpi / POINTS_PER_INCH,
                    0,
                    0,
                    0,
                    weight.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    DEFAULT_PITCH.0 as u32,
                    w!("Segoe UI"),
                )
            })
            .collect();
            if new_fonts.iter().any(|font| font.0.is_null()) {
                delete_fonts(new_fonts);
                return Err(Error::new("Cannot create the Windows interface font"));
            }
            for (&id, &control) in &self.controls {
                let font = if id >= BUNDLE {
                    FONT_SETTINGS
                } else {
                    FONT_BODY
                };
                send(control, WM_SETFONT, new_fonts[font].0 as usize, 1);
            }
            delete_fonts(std::mem::replace(&mut self.fonts, new_fonts));

            let flags = OwnedImages(ImageList_Create(
                FLAG_WIDTH * main_dpi / BASE_DPI,
                FLAG_HEIGHT * main_dpi / BASE_DPI,
                ILC_COLOR24 | ILC_MASK,
                Language::ALL.len() as i32,
                0,
            ));
            if flags.0 == HIMAGELIST::default() {
                return Err(Error::new("Cannot create flag image list"));
            }
            let instance = HINSTANCE(GetModuleHandleW(None).map_err(os_error)?.0);
            for index in 0..Language::ALL.len() {
                let bitmap = LoadImageW(
                    Some(instance),
                    PCWSTR((FLAG_FIRST_RESOURCE + index) as *const u16),
                    IMAGE_BITMAP,
                    FLAG_WIDTH * main_dpi / BASE_DPI,
                    FLAG_HEIGHT * main_dpi / BASE_DPI,
                    LR_CREATEDIBSECTION,
                )
                .map_err(os_error)?;
                let added = ImageList_AddMasked(
                    flags.0,
                    ::windows::Win32::Graphics::Gdi::HBITMAP(bitmap.0),
                    FLAG_TRANSPARENT_COLOR,
                );
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                if added < 0 {
                    return Err(Error::new("Cannot add a language flag"));
                }
            }
            send(self.control(LANGUAGE), CBEM_SETIMAGELIST, 0, flags.0.0);
            self.flags = flags;
        }

        send(self.control(LANGUAGE), CB_RESETCONTENT, 0, 0);
        for (index, language) in Language::ALL.iter().enumerate() {
            let mut text = wide(language.name());
            let item = COMBOBOXEXITEMW {
                mask: CBEIF_TEXT | CBEIF_IMAGE | CBEIF_SELECTEDIMAGE,
                iItem: index as isize,
                pszText: ::windows::core::PWSTR(text.as_mut_ptr()),
                iImage: index as i32,
                iSelectedImage: index as i32,
                ..Default::default()
            };
            send(
                self.control(LANGUAGE),
                CBEM_INSERTITEMW,
                0,
                (&item as *const COMBOBOXEXITEMW) as isize,
            );
        }
        if let Some(index) = Language::ALL.iter().position(|l| *l == self.language()) {
            send(self.control(LANGUAGE), CB_SETCURSEL, index, 0);
        }
        Ok(())
    }
    fn layout(&self, window: HWND) {
        let mut rect = RECT::default();

        // SAFETY: this window and all controls are owned by the current UI thread.
        // Layout reads its client rectangle and changes only child geometry.
        // No temporary pointer is retained by these synchronous native calls.
        unsafe {
            let _ = GetClientRect(window, &mut rect);
        }

        let dpi = dpi(window);
        let scale = |value: i32| value * dpi / BASE_DPI;
        let width = rect.right * BASE_DPI / dpi;
        let height = rect.bottom * BASE_DPI / dpi;
        let content_width = width - MARGIN * 2;

        // SAFETY: every control belongs to this UI thread and its live parent.
        // Positions are scalar DPI-adjusted bounds, not borrowed memory.
        // Keeping z-order and activation unchanged also preserves keyboard focus.
        let place = |id, x, y, w: i32, h| unsafe {
            let control = self.control(id);
            let _ = SetWindowPos(
                control,
                None,
                scale(x),
                scale(y),
                scale(w.max(GAP)),
                scale(h),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if id == LANGUAGE {
                // ComboBoxEx sizes its visible wrapper independently of its list.
                // Resize the embedded combo so the popup keeps its requested height.
                // Reuse the same DPI-scaled bounds after layout and monitor changes.
                let combo = HWND(send(control, CBEM_GETCOMBOCONTROL, 0, 0) as *mut _);
                let _ = SetWindowPos(
                    combo,
                    None,
                    0,
                    0,
                    scale(w.max(GAP)),
                    scale(h),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        };

        if window == self.windows.main {
            let subtitle_top = MARGIN + CONTROL_HEIGHT + GAP;
            let source_top = subtitle_top + LINE_HEIGHT + GAP * 2;
            let source_control = source_top + LINE_HEIGHT + GAP / 2;
            let output_top = source_control + CONTROL_HEIGHT + MARGIN;
            let output_control = output_top + LINE_HEIGHT + GAP / 2;
            let hint_top = output_control + CONTROL_HEIGHT + GAP;
            let strength_top = hint_top + LINE_HEIGHT * 2 + GAP;
            let strength_control = strength_top + LINE_HEIGHT + GAP / 2;
            let echo_top = strength_control + CONTROL_HEIGHT + MARGIN;
            let echo_control = echo_top + LINE_HEIGHT + GAP / 2;
            let signal_top = echo_control + CONTROL_HEIGHT + MARGIN;
            let meter_top = signal_top + LINE_HEIGHT + GAP;
            let message_top = meter_top + METER_HEIGHT + GAP;
            let buttons_top = height - MARGIN - CONTROL_HEIGHT;

            place(
                LANGUAGE,
                width - MARGIN - LANGUAGE_WIDTH,
                MARGIN,
                LANGUAGE_WIDTH,
                DROPDOWN_HEIGHT,
            );
            place(SUBTITLE, MARGIN, subtitle_top, content_width, LINE_HEIGHT);
            place(SOURCE_LABEL, MARGIN, source_top, content_width, LINE_HEIGHT);
            place(
                SOURCE,
                MARGIN,
                source_control,
                content_width - REFRESH_WIDTH - GAP,
                DROPDOWN_HEIGHT,
            );
            place(
                REFRESH,
                width - MARGIN - REFRESH_WIDTH,
                source_control,
                REFRESH_WIDTH,
                CONTROL_HEIGHT,
            );
            place(OUTPUT_LABEL, MARGIN, output_top, content_width, LINE_HEIGHT);
            place(
                OUTPUT,
                MARGIN,
                output_control,
                content_width,
                DROPDOWN_HEIGHT,
            );
            place(ROUTE_HINT, MARGIN, hint_top, content_width, LINE_HEIGHT * 2);
            place(
                STRENGTH_LABEL,
                MARGIN,
                strength_top,
                content_width,
                LINE_HEIGHT,
            );
            place(
                STRENGTH,
                MARGIN,
                strength_control,
                content_width - PERCENT_WIDTH - GAP,
                CONTROL_HEIGHT,
            );
            place(
                PERCENT,
                width - MARGIN - PERCENT_WIDTH,
                strength_control,
                PERCENT_WIDTH,
                LINE_HEIGHT,
            );
            place(ECHO_LABEL, MARGIN, echo_top, content_width, LINE_HEIGHT);
            place(
                ECHO,
                MARGIN,
                echo_control,
                content_width - PERCENT_WIDTH - GAP,
                CONTROL_HEIGHT,
            );
            place(
                ECHO_PERCENT,
                width - MARGIN - PERCENT_WIDTH,
                echo_control,
                PERCENT_WIDTH,
                CONTROL_HEIGHT,
            );
            place(SIGNAL_LABEL, MARGIN, signal_top, content_width, LINE_HEIGHT);
            place(METER, MARGIN, meter_top, content_width, METER_HEIGHT);
            place(
                STATUS,
                MARGIN,
                message_top,
                content_width,
                (buttons_top - message_top - GAP).max(LINE_HEIGHT),
            );
            place(SETTINGS, MARGIN, buttons_top, ACTION_WIDTH, CONTROL_HEIGHT);
            place(
                START,
                width - MARGIN - ACTION_WIDTH,
                buttons_top,
                ACTION_WIDTH,
                CONTROL_HEIGHT,
            );
        } else {
            let bundle_top = MARGIN + LINE_HEIGHT + GAP;
            let cancel_top = bundle_top + CONTROL_HEIGHT + GAP;
            let adapter_top = cancel_top + CONTROL_HEIGHT + MARGIN;
            let adapter_control = adapter_top + LINE_HEIGHT + GAP;
            let delay_top = adapter_control + CONTROL_HEIGHT + GAP;

            place(MODEL_LABEL, MARGIN, MARGIN, content_width, LINE_HEIGHT);
            place(
                BUNDLE,
                MARGIN,
                bundle_top,
                content_width - BROWSE_WIDTH * 2 - GAP * 2,
                CONTROL_HEIGHT,
            );
            place(
                BROWSE,
                width - MARGIN - BROWSE_WIDTH * 2 - GAP,
                bundle_top,
                BROWSE_WIDTH,
                CONTROL_HEIGHT,
            );
            place(
                INSTALLER,
                width - MARGIN - BROWSE_WIDTH,
                bundle_top,
                BROWSE_WIDTH,
                CONTROL_HEIGHT,
            );
            place(
                CANCEL_PICKER,
                width - MARGIN - ACTION_WIDTH,
                cancel_top,
                ACTION_WIDTH,
                CONTROL_HEIGHT,
            );
            place(
                ADAPTER_LABEL,
                MARGIN,
                adapter_top,
                content_width,
                LINE_HEIGHT,
            );
            place(
                ADAPTER,
                MARGIN,
                adapter_control,
                content_width,
                DROPDOWN_HEIGHT,
            );
            place(DELAY, MARGIN, delay_top, content_width, LINE_HEIGHT * 2);
            place(
                ECHO_TIP,
                MARGIN,
                delay_top + LINE_HEIGHT * 2 + GAP,
                content_width,
                LINE_HEIGHT * 3,
            );
            place(
                SAVE,
                width - MARGIN - SETTINGS_ACTION_WIDTH,
                height - MARGIN - CONTROL_HEIGHT,
                SETTINGS_ACTION_WIDTH,
                CONTROL_HEIGHT,
            );
        }
    }
    fn translate(&mut self) {
        for (id, key) in [
            (SOURCE_LABEL, Text::Microphone),
            (OUTPUT_LABEL, Text::WindowsOutput),
            (STRENGTH_LABEL, Text::Removal),
            (SIGNAL_LABEL, Text::Signal),
            (SETTINGS, Text::Settings),
            (REFRESH, Text::Refresh),
            (SUBTITLE, Text::LocalProcessing),
            (ROUTE_HINT, Text::WindowsRouteHint),
            (MODEL_LABEL, Text::ModelFolder),
            (BROWSE, Text::Browse),
            (INSTALLER, Text::Installer),
            (ADAPTER_LABEL, Text::Device),
            (SAVE, Text::Save),
            (CANCEL_PICKER, Text::Cancel),
            (DELAY, Text::Delay),
            (ECHO_LABEL, Text::EchoReduction),
            (ECHO_TIP, Text::EchoTip),
        ] {
            set_text(self.control(id), self.text(key));
        }
        set_text(self.windows.settings, self.text(Text::Settings));
        self.update();
    }
    fn update(&self) {
        set_text(
            self.control(ECHO_PERCENT),
            &format!(
                "{}%",
                (self.preferences.active.echo_reduction * PERCENT_MAX as f32).round()
            ),
        );
        set_text(
            self.control(START),
            self.text(if self.live.is_some() {
                Text::Stop
            } else {
                Text::Start
            }),
        );
        set_text(
            self.control(PERCENT),
            &format!(
                "{}%",
                (self.preferences.active.strength * PERCENT_MAX as f32).round()
            ),
        );
        set_text(
            self.control(STATUS),
            if self.message.is_empty() {
                self.text(if self.running {
                    Text::Processing
                } else if self.live.is_some() {
                    Text::Starting
                } else {
                    Text::Stopped
                })
            } else {
                &self.message
            },
        );
        let idle = self.live.is_none();

        // SAFETY: all control handles belong to this App's live window hierarchy.
        // EnableWindow changes only UI interaction state on the owning thread.
        // Worker state arrives through channels and never touches these handles.
        unsafe {
            for id in [SOURCE, OUTPUT, BUNDLE, ADAPTER] {
                let _ = EnableWindow(self.control(id), idle);
            }
            let _ = EnableWindow(self.control(REFRESH), idle && !self.discovering());
            for id in [BROWSE, INSTALLER] {
                let _ = EnableWindow(self.control(id), idle && self.picker.is_none());
            }
            let _ = EnableWindow(self.control(CANCEL_PICKER), self.picker.is_some());
            let _ = EnableWindow(
                self.control(START),
                self.live.is_some() || (!self.discovering() && self.picker.is_none()),
            );
        }
        if self
            .levels_at
            .is_none_or(|at| at.elapsed() > METER_STALE_AFTER)
        {
            send(self.control(METER), PBM_SETPOS, 0, 0);
        }
    }
    fn save(&mut self, explicit: bool) -> Result<()> {
        if !self.can_save && !explicit {
            return Ok(());
        }
        self.preferences.active.bundle = get_text(self.control(BUNDLE));
        self.preferences.save(&self.path)?;
        self.can_save = true;
        Ok(())
    }
    fn request_close(&mut self, window: HWND) -> Result<()> {
        if let Err(error) = self.save(false) {
            self.message = error.to_string();
        }
        if window == self.windows.settings {
            self.picker.take();

            // SAFETY: this is the settings window owned by the current UI thread.
            // Hiding preserves its native controls for the next Settings action.
            // Its HWND remains owned until App destroys the complete hierarchy.
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
            }
        } else {
            self.stop();
            self.picker.take();

            // SAFETY: this thread owns the active native message pump.
            // Saving is best-effort; a preference failure must not trap the window.
            // App's owner then destroys windows before dependent graphics resources.
            unsafe {
                PostQuitMessage(0);
            }
        }
        Ok(())
    }
    fn discovering(&self) -> bool {
        self.devices_task.is_some() || self.adapters_task.is_some()
    }
    fn refresh(&mut self) {
        if self.discovering() || self.live.is_some() {
            return;
        }
        match super::task::JsonTask::start(&["__windows-devices"], DISCOVERY_TIMEOUT) {
            Ok(task) => self.devices_task = Some(task),
            Err(error) => self.message = error.to_string(),
        }
        match super::task::JsonTask::start(&["gpu-info"], DISCOVERY_TIMEOUT) {
            Ok(task) => self.adapters_task = Some(task),
            Err(error) => self.message = error.to_string(),
        }
    }
    fn stop(&mut self) {
        self.live.take();
        self.running = false;
        self.levels_at = None;
    }
    fn start(&mut self) -> Result<()> {
        self.preferences.active.bundle = get_text(self.control(BUNDLE));
        self.preferences.validate()?;
        if self.adapters.is_empty() {
            return Err(Error::new(
                "No supported AMD Vulkan adapter was found. Check the AMD driver and refresh before starting.",
            ));
        }
        let active = &self.preferences.active;
        if active.bundle.is_empty() {
            return Err(Error::new(self.text(Text::ChooseBundle)));
        }
        if active.source.is_empty() {
            return Err(Error::new(self.text(Text::ChooseSource)));
        }
        if active.render.is_empty() {
            return Err(Error::new(self.text(Text::ChooseOutput)));
        }
        let control = self
            .path
            .with_file_name(format!("windows-control-{}.json", std::process::id()));
        ProcessingControl::write(&control, active)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("__windows-live")
            .args([&active.bundle, &active.source, &active.render])
            .arg(&control)
            .arg(&active.adapter)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut process = match OwnedChild::spawn(&mut command) {
            Ok(process) => process,
            Err(error) => {
                let _ = std::fs::remove_file(control);
                return Err(error);
            }
        };
        let id = process.process()?.id();
        let stderr = process
            .process()?
            .stderr
            .take()
            .ok_or_else(|| Error::new("Missing audio event pipe"))?;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if line.len() > MAX_CHILD_EVENT_BYTES {
                    continue;
                }
                let event = match serde_json::from_str(&line) {
                    Ok(value) => Event::Worker(id, value),
                    Err(_) => Event::Log(id, line),
                };
                let _ = sender.try_send(event);
            }
        });
        self.live = Some(Live {
            process,
            control,
            id,
        });
        self.message.clear();
        self.save(false)?;
        Ok(())
    }
    fn browse(&mut self, installer: bool) -> Result<()> {
        if self.picker.is_some() || self.live.is_some() {
            return Ok(());
        }
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("__windows-pick-model-folder")
            .arg(get_text(self.control(BUNDLE)))
            .arg(self.text(if installer {
                Text::InstallerTitle
            } else {
                Text::FolderTitle
            }))
            .arg(if installer { "installer" } else { "folder" })
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        self.picker = Some(OwnedChild::spawn(&mut command)?);
        self.message = if installer {
            self.text(Text::SelectingModel).into()
        } else {
            String::new()
        };
        Ok(())
    }
    fn tick(&mut self) -> Result<()> {
        if let Some(result) = self
            .devices_task
            .as_ref()
            .and_then(super::task::JsonTask::poll)
        {
            self.devices_task.take();
            let result =
                result.and_then(|value| serde_json::from_value(value).map_err(Error::from));
            let _ = self.sender.try_send(Event::Devices(result));
        }
        if let Some(result) = self
            .adapters_task
            .as_ref()
            .and_then(super::task::JsonTask::poll)
        {
            self.adapters_task.take();
            let result =
                result.and_then(|value| serde_json::from_value(value).map_err(Error::from));
            let _ = self.sender.try_send(Event::Adapters(result));
        }
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Devices(result) => match result {
                    Ok(devices) => {
                        if !devices.warnings.is_empty() {
                            self.message = devices.warnings.join("; ");
                        }
                        self.inputs = devices.inputs;
                        self.outputs = devices.outputs;
                        let input = self
                            .inputs
                            .iter()
                            .position(|d| d.id == self.preferences.active.source)
                            .or_else(|| (!self.inputs.is_empty()).then_some(0));
                        self.preferences.active.source =
                            input.map(|i| self.inputs[i].id.clone()).unwrap_or_default();
                        let output = self
                            .outputs
                            .iter()
                            .position(|d| d.id == self.preferences.active.render);
                        if output.is_none() {
                            self.preferences.active.render.clear();
                        }
                        fill_combo(
                            self.control(SOURCE),
                            &self
                                .inputs
                                .iter()
                                .map(|d| d.name.clone())
                                .collect::<Vec<_>>(),
                            input,
                        );
                        fill_combo(
                            self.control(OUTPUT),
                            &self
                                .outputs
                                .iter()
                                .map(|d| d.name.clone())
                                .collect::<Vec<_>>(),
                            output,
                        );
                    }
                    Err(error) => self.message = format!("Audio device discovery: {error}"),
                },
                Event::Adapters(result) => match result {
                    Ok(adapters) => {
                        self.adapters = adapters
                            .into_iter()
                            .filter(|a| {
                                a.vendor == 0x1002
                                    && matches!(
                                        a.device_type.as_str(),
                                        "DiscreteGpu" | "IntegratedGpu"
                                    )
                            })
                            .map(|a| a.name)
                            .collect();
                        let names = std::iter::once(self.text(Text::Automatic).to_owned())
                            .chain(self.adapters.iter().cloned())
                            .collect::<Vec<_>>();
                        let adapter = self
                            .adapters
                            .iter()
                            .position(|a| *a == self.preferences.active.adapter)
                            .map_or(0, |i| i + 1);
                        fill_combo(self.control(ADAPTER), &names, Some(adapter));
                    }
                    Err(error) => self.message = format!("Graphics discovery: {error}"),
                },
                Event::Worker(id, event) if self.live.as_ref().is_some_and(|l| l.id == id) => {
                    match event["event"].as_str() {
                        Some("started") => {
                            self.running = true;
                            self.message.clear();
                        }
                        Some("levels") => {
                            if let (Some(rms), Some(peak)) =
                                (event["output_rms"].as_f64(), event["output_peak"].as_f64())
                                && rms.is_finite()
                                && peak.is_finite()
                                && rms >= 0.0
                                && peak >= 0.0
                            {
                                self.levels_at = Some(Instant::now());
                                let db = 20.0 * rms.max(LOG_FLOOR).log10();
                                send(
                                    self.control(METER),
                                    PBM_SETPOS,
                                    ((1.0 - db / METER_FLOOR_DBFS).clamp(0.0, 1.0)
                                        * METER_STEPS as f64)
                                        as usize,
                                    0,
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Event::Log(id, line) if self.live.as_ref().is_some_and(|l| l.id == id) => {
                    self.message = line
                }
                _ => {}
            }
        }
        if let Some(live) = &mut self.live
            && let Some(status) = live.process.process()?.try_wait()?
        {
            self.stop();
            if !status.success() && self.message.is_empty() {
                self.message = self.text(Text::StoppedError).to_owned();
            }
        }
        let picker_status = self
            .picker
            .as_mut()
            .map(|picker| picker.process()?.try_wait().map_err(Error::from))
            .transpose()?
            .flatten();
        if let Some(status) = picker_status {
            let mut picker = self
                .picker
                .take()
                .ok_or_else(|| Error::new("Folder picker owner is unavailable"))?;
            if status.success() {
                self.message.clear();
                let mut output = String::new();
                if let Some(stdout) = picker.process()?.stdout.take() {
                    stdout
                        .take((MAX_PICKER_RESPONSE_BYTES + 1) as u64)
                        .read_to_string(&mut output)?;
                }
                if output.len() > MAX_PICKER_RESPONSE_BYTES {
                    return Err(Error::new("Folder picker response is too large"));
                }
                if let Some(path) = crate::controls::decode_picker_response(output.as_bytes())? {
                    set_text(self.control(BUNDLE), &path.to_string_lossy());
                    self.save(false)?;
                }
            } else {
                self.message = self.text(Text::PickerError).to_owned();
            }
        }
        self.update();
        Ok(())
    }
    fn command(&mut self, id: u16, notification: u16) -> Result<()> {
        match id {
            START => {
                if self.live.is_some() {
                    self.stop();
                    self.message.clear();
                } else {
                    self.start()?;
                }
            }
            REFRESH => self.refresh(),
            SETTINGS => {
                // SAFETY: the settings HWND is owned and created on this UI thread.
                // It stays alive while hidden, so showing it does not recreate controls.
                // Foreground activation neither transfers nor retains window ownership.
                unsafe {
                    let _ = ShowWindow(self.windows.settings, SW_SHOW);
                    let _ = SetForegroundWindow(self.windows.settings);
                }
            }
            BROWSE | INSTALLER => self.browse(id == INSTALLER)?,
            CANCEL_PICKER => {
                self.picker.take();
            }
            SAVE => {
                self.save(true)?;
                self.message = self.text(Text::SettingsSaved).to_owned();
            }
            SOURCE | OUTPUT | ADAPTER | LANGUAGE if notification == CBN_SELCHANGE as u16 => {
                if let Some(index) = selected(self.control(id)) {
                    match id {
                        SOURCE => {
                            if let Some(d) = self.inputs.get(index) {
                                self.preferences.active.source = d.id.clone();
                            }
                        }
                        OUTPUT => {
                            if let Some(d) = self.outputs.get(index) {
                                self.preferences.active.render = d.id.clone();
                            }
                        }
                        ADAPTER => {
                            self.preferences.active.adapter = index
                                .checked_sub(1)
                                .and_then(|i| self.adapters.get(i))
                                .cloned()
                                .unwrap_or_default()
                        }
                        LANGUAGE => {
                            if let Some(language) = Language::ALL.get(index) {
                                self.preferences.language = *language;
                                self.translate();
                            }
                        }
                        _ => {}
                    }
                    self.save(false)?;
                }
            }
            _ => {}
        }
        self.update();
        Ok(())
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.devices_task.take();
        self.adapters_task.take();
        self.live.take();
        self.picker.take();
        // SAFETY: controls and fonts belong to this thread's window hierarchy.
        // Detach application fonts before controls release their cached DCs.
        // The borrowed system font stays owned by Windows and is never deleted.
        let system_font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        for control in self.controls.values() {
            send(*control, WM_SETFONT, system_font.0 as usize, 0);
        }
        self.windows.destroy();
        delete_fonts(self.fonts.drain(..));
    }
}

// SAFETY: Windows invokes this callback with its documented message parameters.
// Pointer-bearing messages are interpreted only in their matching message branch.
// Catching Rust panics prevents unwinding across the native callback boundary.
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = std::panic::catch_unwind(|| {
        APP.with(|slot| {
            // Sending messages to child controls can synchronously re-enter this pump.
            // Let the platform handle those nested notifications while App is borrowed.
            // No callback may hold a second mutable reference to the desktop state.
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return None;
            };
            let app = slot.as_mut()?;
            let result = match message {
                WM_COMMAND => app.command(
                    (wparam.0 & 0xffff) as u16,
                    ((wparam.0 >> 16) & 0xffff) as u16,
                ),
                WM_TIMER => app.tick(),
                WM_GETMINMAXINFO => {
                    let scale = dpi(window);
                    let (width, height) = if window == app.windows.settings {
                        (SETTINGS_WIDTH, SETTINGS_HEIGHT)
                    } else {
                        (MAIN_WIDTH, MAIN_HEIGHT)
                    };

                    // SAFETY: Windows supplies a writable MINMAXINFO for this message.
                    // It remains valid only during this synchronous callback.
                    // Only minimum tracking dimensions for this owned window change.
                    unsafe {
                        let info = &mut *(lparam.0 as *mut MINMAXINFO);
                        info.ptMinTrackSize.x = width * scale / BASE_DPI;
                        info.ptMinTrackSize.y = height * scale / BASE_DPI;
                    }
                    Ok(())
                }
                WM_HSCROLL
                    if [STRENGTH, ECHO]
                        .iter()
                        .any(|id| lparam.0 == app.control(*id).0 as isize) =>
                {
                    // The trackbar get-position message aliases WM_USER in commctrl.h.
                    // windows-rs omits that alias, so use its documented numeric identity.
                    // The returned position is limited by the configured percentage range.
                    let id = if lparam.0 == app.control(ECHO).0 as isize {
                        ECHO
                    } else {
                        STRENGTH
                    };
                    let value = send(app.control(id), WM_USER, 0, 0) as f32 / PERCENT_MAX as f32;
                    if id == ECHO {
                        app.preferences.active.echo_reduction = value;
                    } else {
                        app.preferences.active.strength = value;
                    }
                    let result = if let Some(live) = &app.live {
                        ProcessingControl::write(&live.control, &app.preferences.active)
                    } else {
                        Ok(())
                    };
                    app.update();
                    result.and_then(|_| app.save(false))
                }
                WM_SIZE => {
                    app.layout(window);
                    Ok(())
                }
                WM_SYSCOLORCHANGE => {
                    // SAFETY: all controls are owned by this UI thread.
                    // Forward system palette changes to this window's common controls.
                    // System-owned brushes track the new palette without allocation.
                    unsafe {
                        for &control in app.controls.values() {
                            if IsChild(window, control).as_bool() {
                                send(control, WM_SYSCOLORCHANGE, 0, 0);
                            }
                        }
                        let _ = RedrawWindow(
                            Some(window),
                            None,
                            None,
                            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                        );
                    }
                    Ok(())
                }
                WM_DPICHANGED => {
                    // SAFETY: Windows supplies a RECT valid for this DPI-change callback.
                    // The rectangle is copied before updating this UI-thread-owned window.
                    // SetWindowPos consumes scalar bounds and retains no borrowed pointer.
                    unsafe {
                        let rect = *(lparam.0 as *const RECT);
                        let _ = SetWindowPos(
                            window,
                            None,
                            rect.left,
                            rect.top,
                            rect.right - rect.left,
                            rect.bottom - rect.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    let result = app.rebuild_fonts();
                    app.layout(window);
                    result
                }
                WM_CLOSE => app.request_close(window),
                _ => return None,
            };
            if let Err(error) = result {
                app.message = error.to_string();
                app.update();
            }
            Some(LRESULT(0))
        })
    });
    match handled {
        Ok(Some(result)) => result,
        Ok(None) => {
            // SAFETY: unhandled parameters are forwarded unchanged to Windows.
            // The platform owns any message-specific memory for this callback.
            // No Rust borrow remains while the default procedure runs.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        Err(_) => {
            // SAFETY: this callback runs on the UI message-pump thread.
            // Request its failure exit after catching the panic locally.
            // The outer owner still performs window and resource destruction.
            unsafe {
                PostQuitMessage(1);
            }
            LRESULT(0)
        }
    }
}

fn register_window_class() -> Result<()> {
    // SAFETY: embedded resources belong to this executable's module instance.
    // Class and callback data remain static for the process lifetime.
    // Repeated initialization reuses only this same application's registered class.
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).map_err(os_error)?.0);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES | ICC_PROGRESS_CLASS | ICC_USEREX_CLASSES,
        };
        InitCommonControlsEx(&controls).ok().map_err(os_error)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS,
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(os_error)?,
            hIcon: LoadIconW(Some(instance), PCWSTR(APP_ICON_RESOURCE as *const u16))
                .map_err(os_error)?,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(Error::new("Cannot register the Clearwave window"));
        }
    }
    Ok(())
}

pub fn launch(args: &[PathBuf]) -> Result<()> {
    let options = DesktopOptions::parse(args)?;

    let _apartment = Apartment::new()?;
    // SAFETY: this thread owns the apartment, window hierarchy and message pump.
    // All messages and handles remain scoped to that hierarchy until App drops.
    // Exiting any error path releases RAII-owned windows before graphics resources.
    unsafe {
        register_window_class()?;
        let windows = OwnedWindows::create()?;
        let (main, settings) = (windows.main, windows.settings);
        let dpi = dpi(main);
        SetWindowPos(
            main,
            None,
            0,
            0,
            MAIN_WIDTH * dpi / BASE_DPI,
            MAIN_HEIGHT * dpi / BASE_DPI,
            SWP_NOMOVE | SWP_NOZORDER,
        )
        .map_err(os_error)?;
        SetWindowPos(
            settings,
            None,
            0,
            0,
            SETTINGS_WIDTH * dpi / BASE_DPI,
            SETTINGS_HEIGHT * dpi / BASE_DPI,
            SWP_NOMOVE | SWP_NOZORDER,
        )
        .map_err(os_error)?;
        let mut app = App::create(windows, options.settings, options.bundle)?;
        app.refresh();
        APP.with(|slot| *slot.borrow_mut() = Some(app));
        SetTimer(
            Some(main),
            TIMER_ID,
            UI_REFRESH_INTERVAL.as_millis() as u32,
            None,
        );
        let _ = ShowWindow(main, SW_SHOW);
        let mut message = MSG::default();
        let message_error = loop {
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status <= 0 {
                break status < 0;
            }
            let target = if IsWindowVisible(settings).as_bool()
                && (message.hwnd == settings || IsChild(settings, message.hwnd).as_bool())
            {
                settings
            } else {
                main
            };
            if !IsDialogMessageW(target, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        };
        let _ = KillTimer(Some(main), TIMER_ID);
        let app = APP.with(|slot| slot.borrow_mut().take());
        drop(app);
        if message_error || message.wParam.0 != 0 {
            return Err(Error::new("Windows interface stopped unexpectedly"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::Win32::System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources};
    const RESOURCE_CHECK_CYCLES: usize = 8;
    const MAX_RETAINED_GDI_OBJECTS: u32 = 4;
    const TEST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(2);

    fn take_quit_message() -> bool {
        let deadline = Instant::now() + TEST_MESSAGE_TIMEOUT;
        // SAFETY: the fixture owns this thread's native windows and message queue.
        // WM_QUIT is generated at low priority after pending messages are drained.
        // Dispatch completes native painting/cleanup before checking the quit flag.
        unsafe {
            let mut message = MSG::default();
            while Instant::now() < deadline {
                if PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    if message.message == WM_QUIT {
                        return true;
                    }
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                } else {
                    std::thread::yield_now();
                }
            }
        }
        false
    }

    struct SettingsDirectory(PathBuf);
    impl SettingsDirectory {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("clearwave-windows-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for SettingsDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_windows_preserve_settings_and_release_their_resources() {
        let _apartment = Apartment::new().unwrap();
        register_window_class().unwrap();
        let directory = SettingsDirectory::new();
        let windows = OwnedWindows::create().unwrap();
        let (main, settings) = (windows.main, windows.settings);
        let mut app = App::create(
            windows,
            directory.0.join("desktop.json"),
            Some(PathBuf::from(r"C:\models\voice")),
        )
        .unwrap();

        app.preferences.active.source = "capture-endpoint".into();
        app.preferences.active.render = "render-endpoint".into();
        app.preferences.active.strength = 0.42;
        app.save(true).unwrap();
        set_text(app.control(BUNDLE), r"C:\models\updated");
        app.preferences.active.strength = 0.73;
        app.save(false).unwrap();
        let saved = Preferences::load(&app.path).unwrap();
        assert_eq!(saved.active.bundle, r"C:\models\updated");
        assert_eq!(saved.active.source, "capture-endpoint");
        assert_eq!(saved.active.render, "render-endpoint");
        assert_eq!(saved.active.strength, 0.73);

        app.path = directory.0.clone();
        assert!(app.request_close(main).is_ok());

        // SAFETY: these handles belong to the App retained on this test thread.
        // Native traversal only queries siblings, and PeekMessage does not dispatch.
        // A failed save must still request quit and release the owned windows.
        unsafe {
            assert!(IsWindow(Some(main)).as_bool());
            assert!(IsWindow(Some(settings)).as_bool());
            assert!(take_quit_message());
            let expected = [
                LANGUAGE, SOURCE, REFRESH, OUTPUT, STRENGTH, ECHO, SETTINGS, START,
            ];
            let mut previous = None;
            for id in expected {
                let next = GetNextDlgTabItem(main, previous, false).unwrap();
                let control = app.control(id);
                assert!(next == control || IsChild(control, next).as_bool());
                previous = Some(next);
            }
            drop(app);
            assert!(!IsWindow(Some(main)).as_bool());
            assert!(!IsWindow(Some(settings)).as_bool());
            let baseline = GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS);
            for _ in 0..RESOURCE_CHECK_CYCLES {
                let windows = OwnedWindows::create().unwrap();
                let app =
                    App::create(windows, directory.0.join("resource-check.json"), None).unwrap();
                drop(app);
            }
            let retained = GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS);
            assert!(
                retained <= baseline + MAX_RETAINED_GDI_OBJECTS,
                "GDI resources grew from {baseline} to {retained}"
            );
        }
    }

    #[test]
    fn native_echo_slider_saves_independently_of_noise_strength() {
        let _apartment = Apartment::new().unwrap();
        register_window_class().unwrap();
        let directory = SettingsDirectory::new();
        let windows = OwnedWindows::create().unwrap();
        let main = windows.main;
        let app = App::create(windows, directory.0.join("echo-slider.json"), None).unwrap();
        let slider = app.control(ECHO);
        assert_eq!(send(slider, WM_USER, 0, 0), 0);
        APP.with(|slot| *slot.borrow_mut() = Some(app));
        for position in [37, 100, 0] {
            send(slider, TBM_SETPOS, 1, position);
            send(main, WM_HSCROLL, 0, slider.0 as isize);
            APP.with(|slot| {
                let slot = slot.borrow();
                let app = slot.as_ref().unwrap();
                let preferences = Preferences::load(&app.path).unwrap();
                assert_eq!(
                    preferences.active.echo_reduction,
                    position as f32 / PERCENT_MAX as f32
                );
                assert_eq!(preferences.active.strength, 1.0);
                assert_eq!(get_text(app.control(ECHO_PERCENT)), format!("{position}%"));
            });
        }
        drop(APP.with(|slot| slot.borrow_mut().take()));
    }

    #[test]
    fn native_language_selection_translates_and_saves_all_five_languages() {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_HOME};

        let _apartment = Apartment::new().unwrap();
        register_window_class().unwrap();
        let directory = SettingsDirectory::new();
        let windows = OwnedWindows::create().unwrap();
        let main = windows.main;
        let app = App::create(windows, directory.0.join("desktop.json"), None).unwrap();
        let language = app.control(LANGUAGE);
        let combo = HWND(send(language, CBEM_GETCOMBOCONTROL, 0, 0) as *mut _);

        assert!(!combo.0.is_null());
        assert_eq!(
            send(language, CB_GETCOUNT, 0, 0),
            Language::ALL.len() as isize
        );
        APP.with(|slot| *slot.borrow_mut() = Some(app));

        for (index, expected) in Language::ALL.into_iter().enumerate() {
            send(combo, WM_KEYDOWN, VK_HOME.0 as usize, 0);
            for _ in 0..index {
                send(combo, WM_KEYDOWN, VK_DOWN.0 as usize, 0);
            }
            APP.with(|slot| {
                let slot = slot.borrow();
                let app = slot.as_ref().unwrap();
                assert_eq!(app.language(), expected);
                assert_eq!(
                    get_text(app.control(SETTINGS)),
                    expected.text(Text::Settings)
                );
                assert_eq!(get_text(app.control(START)), expected.text(Text::Start));
                assert_eq!(
                    get_text(app.windows.settings),
                    expected.text(Text::Settings)
                );
                assert_eq!(Preferences::load(&app.path).unwrap().language, expected);
            });
        }

        let mut dropped = RECT::default();
        send(
            language,
            CB_GETDROPPEDCONTROLRECT,
            0,
            (&mut dropped as *mut RECT) as isize,
        );
        let item_height = send(language, CB_GETITEMHEIGHT, 0, 0) as i32;
        let app = APP.with(|slot| slot.borrow_mut().take()).unwrap();
        drop(app);
        assert!(
            dropped.bottom - dropped.top > item_height * Language::ALL.len() as i32,
            "language list height {} cannot show {} items of height {item_height}",
            dropped.bottom - dropped.top,
            Language::ALL.len(),
        );

        // SAFETY: the test has dropped its window owner on this thread.
        // No display or input is retained across test cases.
        // Querying the dead handle confirms normal native lifetime cleanup.
        unsafe { assert!(!IsWindow(Some(main)).as_bool()) };
    }
}

#[cfg(test)]
#[path = "preview.rs"]
mod preview;
