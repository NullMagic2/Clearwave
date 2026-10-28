//! Native desktop controls. Opening the window never starts audio capture.
use crate::{
    Error, Result,
    controls::{
        DESKTOP_EVENT_CAPACITY, DesktopOptions, MAX_CHILD_EVENT_BYTES, MAX_LATENCY_MS,
        METER_STALE_AFTER, MIN_LATENCY_MS, Preferences, ProcessingControl, UI_REFRESH_INTERVAL,
    },
    gpu::AdapterInfo,
    i18n::{Language, Text},
    pipewire::Node,
    voice_balance::{BASS_STEP_DB, MAX_BASS_DB, MIN_BASS_DB},
};
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(400);
const CHILD_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PERCENT_SCALE: f32 = 100.0;
const METER_RANGE_DB: f32 = 60.0;
const MIN_METER_AMPLITUDE: f32 = 1e-6;
const SIGNAL_AMPLITUDE_THRESHOLD: f32 = 1e-5;

enum Event {
    Sources(Result<Vec<Node>>),
    Adapters(Vec<AdapterInfo>),
    Child(u32, serde_json::Value),
    Log(u32, String),
}
struct Live {
    child: Option<Child>,
    control: PathBuf,
    stopping: bool,
    route_ready: bool,
    compute_ready: bool,
}
impl Live {
    fn stop(&mut self) {
        self.stopping = true;
        if let Some(child) = &mut self.child
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        self.stop();
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let deadline = Instant::now() + CHILD_STOP_TIMEOUT;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(CHILD_POLL_INTERVAL);
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
            });
        }
        let _ = std::fs::remove_file(&self.control);
    }
}

#[derive(Default)]
struct Meter {
    rms: f32,
    peak: f32,
}
fn read_child(id: u32, input: impl std::io::Read + Send + 'static, sender: SyncSender<Event>) {
    std::thread::spawn(move || {
        for line in BufReader::new(input).lines() {
            let Ok(line) = line else {
                break;
            };
            if line.len() > MAX_CHILD_EVENT_BYTES {
                continue;
            }
            let event = match serde_json::from_str(&line) {
                Ok(value) => Event::Child(id, value),
                Err(_) => Event::Log(id, line),
            };
            if sender.try_send(event).is_err() {
                continue;
            }
        }
    });
}

pub fn launch(args: &[PathBuf]) -> Result<()> {
    let DesktopOptions { settings, bundle } = DesktopOptions::parse(args)?;

    gtk::init().map_err(|e| Error::new(format!("Cannot open GTK3: {e}")))?;
    let app = Rc::new(RefCell::new(Desktop::new(settings, bundle)));
    let ui = Interface::new()?;
    ui.connect(&app);
    ui.update(&app.borrow());
    ui.window.show_all();
    ui.message.hide();
    let weak = Rc::downgrade(&app);
    let view = ui.clone();
    let timer = glib::timeout_add_local(UI_REFRESH_INTERVAL, move || {
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        {
            let mut state = app.borrow_mut();
            state.events();
            if state
                .dirty
                .is_some_and(|at| at.elapsed() > SETTINGS_SAVE_DELAY)
            {
                state.save(false);
            }
        }
        view.update(&app.borrow());
        if std::env::var("CLEARWAVE_GUI_CLOSE_AFTER")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .is_some_and(|s| s > 0.0 && app.borrow().launched.elapsed().as_secs_f64() > s)
        {
            view.window.close();
        }
        glib::ControlFlow::Continue
    });
    gtk::main();
    timer.remove();
    app.borrow_mut().picker.take();
    app.borrow_mut().live.take();
    app.borrow_mut().save(false);
    Ok(())
}

struct Desktop {
    preferences: Preferences,
    settings_path: PathBuf,
    can_save: bool,
    dirty: Option<Instant>,
    sources: Vec<Node>,
    adapters: Vec<AdapterInfo>,
    sender: SyncSender<Event>,
    events: Receiver<Event>,
    live: Option<Live>,
    picker: Option<crate::folder_picker::FolderPicker>,
    output: Meter,
    last_levels: Option<Instant>,
    output_name: String,
    message: Option<(bool, String)>,
    revision: u64,
    launched: Instant,
}
impl Desktop {
    fn new(settings_path: PathBuf, bundle: Option<PathBuf>) -> Self {
        let (preferences, can_save, message) = match Preferences::load(&settings_path) {
            Ok(p) => (p, true, None),
            Err(e) => (
                Preferences::default(),
                false,
                Some((
                    true,
                    format!(
                        "{}: {e}. {}",
                        Language::default().text(Text::LoadSettingsError),
                        Language::default().text(Text::PreservedSettings)
                    ),
                )),
            ),
        };
        let (sender, events) = mpsc::sync_channel(DESKTOP_EVENT_CAPACITY);
        let mut app = Self {
            preferences,
            settings_path,
            can_save,
            dirty: None,
            sources: Vec::new(),
            adapters: Vec::new(),
            sender,
            events,
            live: None,
            picker: None,
            output: Meter::default(),
            last_levels: None,
            output_name: String::new(),
            message,
            revision: 0,
            launched: Instant::now(),
        };
        if let Some(bundle) = bundle {
            app.preferences.active.bundle = bundle.to_string_lossy().into_owned();
        } else if app.preferences.active.bundle.is_empty()
            && let Ok(runtime) = crate::controls::runtime_directory()
        {
            let path = runtime.join("vendor");
            if path.is_dir() {
                app.preferences.active.bundle = path.to_string_lossy().into_owned();
            }
        }
        app.refresh();
        app
    }
    fn refresh(&self) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(Event::Sources(crate::pipewire::sources()));
        });
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(Event::Adapters(crate::gpu::adapters()));
        });
    }
    fn save(&mut self, explicit: bool) {
        if !self.can_save && !explicit {
            return;
        }
        match self.preferences.save(&self.settings_path) {
            Ok(()) => {
                self.can_save = true;
                self.dirty = None;
                if explicit {
                    self.message = Some((
                        false,
                        self.preferences.language.text(Text::SettingsSaved).into(),
                    ));
                }
            }
            Err(e) => {
                self.message = Some((
                    true,
                    format!("{}: {e}", self.preferences.language.text(Text::CannotSave)),
                ));
                self.dirty = None;
            }
        }
    }
    fn start(&mut self) -> Result<()> {
        let p = &self.preferences.active;
        p.validate()?;
        if p.source.is_empty() {
            return Err(Error::new(
                self.preferences.language.text(Text::ChooseSource),
            ));
        }
        if p.bundle.trim().is_empty() {
            return Err(Error::new(
                self.preferences.language.text(Text::ChooseBundle),
            ));
        }
        let bundle = PathBuf::from(&p.bundle);
        let bundle = if bundle.is_absolute() {
            bundle
        } else {
            std::env::current_dir()?.join(bundle)
        };
        let directory = self
            .settings_path
            .parent()
            .ok_or_else(|| Error::new("Settings have no parent directory"))?;
        let control = directory.join(format!("live-control-{}.json", std::process::id()));
        ProcessingControl::write(&control, p)?;
        let mut command = Command::new("setpriv");
        command
            .args(["--pdeathsig", "KILL", "--"])
            .arg(std::env::current_exe()?)
            .arg("live")
            .arg(bundle)
            .arg(&p.source)
            .args([
                "--strength",
                &p.strength.to_string(),
                "--latency-ms",
                &p.latency_ms.to_string(),
                "--meters",
                "true",
                "--control-file",
            ])
            .arg(&control)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !p.adapter.is_empty() {
            command.arg("--adapter").arg(&p.adapter);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_file(control);
                return Err(e.into());
            }
        };
        let id = child.id();
        if let Some(output) = child.stdout.take() {
            read_child(id, output, self.sender.clone());
        }
        if let Some(output) = child.stderr.take() {
            read_child(id, output, self.sender.clone());
        }
        self.live = Some(Live {
            child: Some(child),
            control,
            stopping: false,
            route_ready: false,
            compute_ready: false,
        });
        self.message = None;
        self.output_name.clear();
        self.last_levels = None;
        self.output = Meter::default();
        self.save(false);
        Ok(())
    }
    fn events(&mut self) {
        if let Some(result) = self.picker.as_ref().and_then(|picker| picker.poll()) {
            self.picker.take();
            self.message = None;
            match result {
                Ok(Some(path)) => {
                    self.preferences.active.bundle = path.to_string_lossy().into_owned();
                    self.revision += 1;
                    self.changed();
                    self.message = None;
                }
                Ok(None) => {}
                Err(error) => {
                    self.message = Some((
                        true,
                        format!(
                            "{}: {error}",
                            self.preferences.language.text(Text::PickerError)
                        ),
                    ))
                }
            }
        }
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Sources(Ok(mut sources)) => {
                    sources.retain(|n| {
                        !n.name.starts_with("clearwave.") && !n.name.starts_with("voice-amd.")
                    });
                    if self.preferences.active.source.is_empty()
                        && let Some(source) =
                            sources.iter().find(|n| !n.virtual_node).or(sources.first())
                    {
                        self.preferences.active.source = source.name.clone();
                    }
                    self.sources = sources;
                    self.revision += 1;
                }
                Event::Sources(Err(e)) => self.message = Some((true, e.to_string())),
                Event::Adapters(adapters) => {
                    self.adapters = adapters
                        .into_iter()
                        .filter(|a| {
                            a.vendor == 0x1002
                                && matches!(a.device_type.as_str(), "DiscreteGpu" | "IntegratedGpu")
                        })
                        .collect();
                    self.revision += 1;
                }
                Event::Log(id, text)
                    if self.live.as_ref().is_some_and(|l| {
                        l.child.as_ref().is_some_and(|child| child.id() == id)
                    }) =>
                {
                    if !text.trim().is_empty() {
                        self.message = Some((true, text));
                    }
                }
                Event::Child(id, value)
                    if self.live.as_ref().is_some_and(|l| {
                        l.child.as_ref().is_some_and(|child| child.id() == id)
                    }) =>
                {
                    match value["event"].as_str().unwrap_or("") {
                        "live_started" => {
                            if let Some(l) = &mut self.live {
                                l.route_ready = true;
                            }
                            self.output_name = value["virtual_microphone"]["description"]
                                .as_str()
                                .unwrap_or("")
                                .into();
                        }
                        "stream_ready" => {
                            if let Some(l) = &mut self.live {
                                l.compute_ready = true;
                            }
                        }
                        "levels" => {
                            let number = |key: &str| {
                                value[key].as_f64().filter(|v| v.is_finite()).unwrap_or(0.0) as f32
                            };
                            self.output = Meter {
                                rms: number("output_rms"),
                                peak: number("output_peak"),
                            };
                            self.last_levels = Some(Instant::now());
                        }
                        "control_error" => {
                            self.message = Some((
                                true,
                                value["message"]
                                    .as_str()
                                    .unwrap_or("Cannot read live controls")
                                    .into(),
                            ))
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        let ended = self
            .live
            .as_mut()
            .and_then(|l| match l.child.as_mut()?.try_wait() {
                Ok(Some(status)) => Some((status.success(), l.stopping)),
                Err(_) => Some((false, l.stopping)),
                _ => None,
            });
        if let Some((success, stopping)) = ended {
            self.live.take();
            self.last_levels = None;
            self.output = Meter::default();
            if !success && !stopping && !self.message.as_ref().is_some_and(|(error, _)| *error) {
                self.message = Some((
                    true,
                    self.preferences.language.text(Text::StoppedError).into(),
                ));
            }
        }
    }
    fn changed(&mut self) {
        self.dirty = Some(Instant::now());
    }
}

/// GTK widgets live exclusively on the main thread.
/// Audio processing and device discovery own their worker resources.
/// Bounded messages transfer plain data without sharing GTK objects.
#[derive(Clone)]
struct Interface {
    window: gtk::Window,
    header: gtk::HeaderBar,
    settings_button: gtk::Button,
    language_button: gtk::MenuButton,
    language_image: gtk::Image,
    language_items: Vec<(gtk::MenuItem, Language)>,
    flags: Vec<(Language, gtk::gdk_pixbuf::Pixbuf)>,
    labels: Vec<(gtk::Label, Text)>,
    buttons: Vec<(gtk::Button, Text)>,
    language: Rc<Cell<Option<Language>>>,
    source: gtk::ComboBoxText,
    refresh: gtk::Button,
    output: gtk::Label,
    start: gtk::Button,
    use_default: gtk::Button,
    strength: gtk::Scale,
    percent: gtk::Label,
    meter: gtk::LevelBar,
    message: gtk::Label,
    settings_message: gtk::Label,
    settings: gtk::Dialog,
    bundle: gtk::Entry,
    browse: gtk::Button,
    installer: gtk::Button,
    adapter: gtk::ComboBoxText,
    latency: gtk::SpinButton,
    echo: gtk::Scale,
    echo_percent: gtk::Label,
    bass: gtk::Scale,
    bass_value: gtk::Label,
    save: gtk::Button,
    updating: Rc<Cell<bool>>,
    revision: Rc<Cell<u64>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label
}

fn vertical(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    widget.set_margin_start(value);
    widget.set_margin_end(value);
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
}

fn slider_row(
    title: &gtk::Label,
    minimum: f64,
    maximum: f64,
    step: f64,
    tip: Text,
) -> (gtk::Box, gtk::Scale, gtk::Label) {
    let panel = vertical(6);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.pack_start(title, true, true, 0);
    let value = label("");
    heading.pack_end(&value, false, false, 0);
    panel.add(&heading);

    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, minimum, maximum, step);
    scale.set_draw_value(false);
    scale.set_tooltip_text(Some(Language::default().text(tip)));
    panel.add(&scale);
    let weak = heading.downgrade();
    scale.connect_size_allocate(move |scale, _| {
        let scale = scale.downgrade();
        let heading = weak.clone();
        glib::idle_add_local_once(move || {
            if let (Some(scale), Some(heading)) = (scale.upgrade(), heading.upgrade()) {
                // Read track geometry after GTK finishes allocating its themed gadgets.
                // Match headings to the trough instead of hard-coding theme padding.
                // Weak references keep pending layout work from retaining closed windows.
                let range = scale.range_rect();
                let left = range.x().max(0);
                let right = (scale.allocated_width() - range.x() - range.width()).max(0);
                let rtl = scale.direction() == gtk::TextDirection::Rtl;
                heading.set_margin_start(if rtl { right } else { left });
                heading.set_margin_end(if rtl { left } else { right });
            }
        });
    });
    (panel, scale, value)
}

impl Interface {
    fn new() -> Result<Self> {
        let mut labels = Vec::new();
        let mut translated = |key: Text| {
            let widget = label(Language::default().text(key));
            labels.push((widget.clone(), key));
            widget
        };

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title(Language::default().text(Text::Title));
        window.set_default_size(560, 530);
        window.set_position(gtk::WindowPosition::Center);

        let header = gtk::HeaderBar::new();
        header.set_title(Some(Language::default().text(Text::Title)));
        header.set_subtitle(Some(Language::default().text(Text::Stopped)));
        header.set_show_close_button(true);
        let settings_button =
            gtk::Button::from_icon_name(Some("emblem-system-symbolic"), gtk::IconSize::Button);
        settings_button.set_tooltip_text(Some(Language::default().text(Text::Settings)));

        let language_button = gtk::MenuButton::new();
        let language_image = gtk::Image::new();
        language_button.add(&language_image);
        let menu = gtk::Menu::new();
        let mut language_items = Vec::new();
        let mut flags = Vec::new();
        for language in Language::ALL {
            let loader = gtk::gdk_pixbuf::PixbufLoader::with_type("svg")
                .map_err(|e| Error::new(format!("Cannot load language flag: {e}")))?;
            loader.set_size(24, 18);
            loader
                .write(language.flag())
                .and_then(|_| loader.close())
                .map_err(|e| Error::new(format!("Cannot decode language flag: {e}")))?;
            let flag = loader
                .pixbuf()
                .ok_or_else(|| Error::new("Language flag has no decoded image"))?;
            let item = gtk::MenuItem::new();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            margins(&row, 4);
            row.add(&gtk::Image::from_pixbuf(Some(&flag)));
            row.add(&label(language.name()));
            item.add(&row);
            menu.append(&item);
            language_items.push((item, language));
            flags.push((language, flag));
        }
        menu.show_all();
        language_button.set_popup(Some(&menu));
        header.pack_start(&language_button);
        header.pack_start(&settings_button);
        window.set_titlebar(Some(&header));

        let content = vertical(22);
        margins(&content, 24);
        window.add(&content);

        let input = vertical(8);
        input.add(&translated(Text::Microphone));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let source = gtk::ComboBoxText::new();
        source.set_hexpand(true);
        if let Some(cell) = source.cells().first() {
            cell.set_property("ellipsize", gtk::pango::EllipsizeMode::End);
            cell.set_property("max-width-chars", 44i32);
        }
        source.set_tooltip_text(Some(Language::default().text(Text::SourceTip)));
        let refresh =
            gtk::Button::from_icon_name(Some("view-refresh-symbolic"), gtk::IconSize::Button);
        refresh.set_tooltip_text(Some(Language::default().text(Text::RefreshTip)));
        row.pack_start(&source, true, true, 0);
        row.pack_start(&refresh, false, false, 0);
        input.add(&row);
        content.add(&input);

        let (removal, strength, percent) = slider_row(
            &translated(Text::Removal),
            0.0,
            f64::from(PERCENT_SCALE),
            1.0,
            Text::StrengthTip,
        );
        strength.set_value(f64::from(PERCENT_SCALE));
        content.add(&removal);

        let (echo_panel, echo, echo_percent) = slider_row(
            &translated(Text::EchoReduction),
            0.0,
            f64::from(PERCENT_SCALE),
            1.0,
            Text::EchoTip,
        );
        content.add(&echo_panel);

        let (bass_panel, bass, bass_value) = slider_row(
            &translated(Text::Bass),
            f64::from(MIN_BASS_DB),
            f64::from(MAX_BASS_DB),
            f64::from(BASS_STEP_DB),
            Text::BassTip,
        );
        bass.connect_change_value(|scale, _, value| {
            let step = f64::from(BASS_STEP_DB);
            scale.set_value((value / step).round() * step);
            glib::Propagation::Stop
        });
        content.add(&bass_panel);

        let signal = vertical(8);
        signal.add(&translated(Text::Signal));
        let bar = gtk::LevelBar::for_interval(0.0, 1.0);
        bar.set_hexpand(true);
        bar.set_value(0.0);
        signal.add(&bar);
        let meter = bar;
        content.add(&signal);

        content.add(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let destination = vertical(8);
        let heading = translated(Text::VirtualMicrophone);
        heading.style_context().add_class("dim-label");
        destination.add(&heading);
        let output = label(Language::default().text(Text::ChooseMicrophone));
        output.set_line_wrap(true);
        output.set_max_width_chars(54);
        output.set_selectable(true);
        destination.add(&output);
        let use_default = gtk::Button::with_label(Language::default().text(Text::Default));
        use_default.set_halign(gtk::Align::Start);
        use_default.set_tooltip_text(Some(Language::default().text(Text::DefaultTip)));
        destination.add(&use_default);
        content.add(&destination);

        let start = gtk::Button::with_label(Language::default().text(Text::Start));
        start.style_context().add_class("suggested-action");
        start.set_height_request(42);
        content.add(&start);
        let message = label("");
        message.set_line_wrap(true);
        message.set_max_width_chars(54);
        message.set_no_show_all(true);
        content.add(&message);

        let settings = gtk::Dialog::with_buttons(
            Some(Language::default().text(Text::Settings)),
            Some(&window),
            gtk::DialogFlags::DESTROY_WITH_PARENT,
            &[(
                Language::default().text(Text::Close),
                gtk::ResponseType::Close,
            )],
        );
        settings.set_default_size(580, 310);
        settings.set_resizable(false);
        settings.connect_response(|dialog, _| dialog.hide());
        settings.connect_delete_event(|dialog, _| {
            dialog.hide();
            glib::Propagation::Stop
        });
        let panel = vertical(18);
        margins(&panel, 20);
        settings.content_area().add(&panel);

        let configuration = vertical(8);
        configuration.add(&translated(Text::ModelFolder));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let bundle = gtk::Entry::new();
        bundle.set_hexpand(true);
        let browse = gtk::Button::with_label(Language::default().text(Text::Browse));
        let installer = gtk::Button::with_label(Language::default().text(Text::Installer));
        row.pack_start(&bundle, true, true, 0);
        row.pack_end(&installer, false, false, 0);
        row.pack_end(&browse, false, false, 0);
        configuration.add(&row);
        configuration.add(&translated(Text::Device));
        let adapter = gtk::ComboBoxText::new();
        configuration.add(&adapter);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.pack_start(&translated(Text::Buffer), true, true, 0);
        let latency =
            gtk::SpinButton::with_range(f64::from(MIN_LATENCY_MS), f64::from(MAX_LATENCY_MS), 1.0);
        row.pack_end(&latency, false, false, 0);
        configuration.add(&row);
        panel.add(&configuration);
        let note = translated(Text::Delay);
        note.set_line_wrap(true);
        note.set_max_width_chars(54);
        note.style_context().add_class("dim-label");
        panel.add(&note);

        let save = gtk::Button::with_label(Language::default().text(Text::Save));
        save.set_halign(gtk::Align::Start);
        panel.add(&save);

        let settings_message = label("");
        settings_message.set_line_wrap(true);
        settings_message.set_max_width_chars(54);
        settings_message.set_no_show_all(true);
        panel.add(&settings_message);
        let dialog = settings.clone();
        settings_button.connect_clicked(move |_| dialog.show_all());

        let mut buttons = vec![
            (use_default.clone(), Text::Default),
            (browse.clone(), Text::Browse),
            (installer.clone(), Text::Installer),
            (save.clone(), Text::Save),
        ];
        if let Some(close) = settings
            .widget_for_response(gtk::ResponseType::Close)
            .and_then(|widget| widget.downcast::<gtk::Button>().ok())
        {
            buttons.push((close, Text::Close));
        }
        Ok(Self {
            window,
            header,
            settings_button,
            language_button,
            language_image,
            language_items,
            flags,
            labels,
            buttons,
            language: Rc::new(Cell::new(None)),
            source,
            refresh,
            output,
            start,
            use_default,
            strength,
            percent,
            meter,
            message,
            settings_message,
            settings,
            bundle,
            browse,
            installer,
            adapter,
            latency,
            echo,
            echo_percent,
            bass,
            bass_value,
            save,
            updating: Rc::new(Cell::new(false)),
            revision: Rc::new(Cell::new(u64::MAX)),
        })
    }

    fn localize(&self, language: Language) {
        for (label, key) in &self.labels {
            label.set_text(language.text(*key));
        }
        for (button, key) in &self.buttons {
            button.set_label(language.text(*key));
        }
        self.window.set_title(language.text(Text::Title));
        self.header.set_title(Some(language.text(Text::Title)));
        self.settings.set_title(language.text(Text::Settings));
        self.settings_button
            .set_tooltip_text(Some(language.text(Text::Settings)));
        self.language_button.set_tooltip_text(Some(&format!(
            "{}: {}",
            language.text(Text::Language),
            language.name()
        )));
        if let Some((_, flag)) = self.flags.iter().find(|(l, _)| *l == language) {
            self.language_image.set_from_pixbuf(Some(flag));
        }
        self.source
            .set_tooltip_text(Some(language.text(Text::SourceTip)));
        self.refresh
            .set_tooltip_text(Some(language.text(Text::RefreshTip)));
        self.strength
            .set_tooltip_text(Some(language.text(Text::StrengthTip)));
        self.echo
            .set_tooltip_text(Some(language.text(Text::EchoTip)));
        self.bass
            .set_tooltip_text(Some(language.text(Text::BassTip)));
        self.use_default
            .set_tooltip_text(Some(language.text(Text::DefaultTip)));
    }

    fn sync_choices(&self, state: &Desktop) {
        self.updating.set(true);
        self.source.remove_all();
        for source in &state.sources {
            self.source.append(Some(&source.name), &source.description);
        }
        self.source
            .set_active_id(Some(&state.preferences.active.source));
        self.adapter.remove_all();
        self.adapter.append(
            Some("auto"),
            state.preferences.language.text(Text::Automatic),
        );
        for adapter in &state.adapters {
            self.adapter.append(Some(&adapter.name), &adapter.name);
        }
        let selected = &state.preferences.active.adapter;
        if !selected.is_empty() && !state.adapters.iter().any(|a| &a.name == selected) {
            self.adapter.append(Some(selected), selected);
        }
        self.adapter.set_active_id(Some(if selected.is_empty() {
            "auto"
        } else {
            selected
        }));
        self.bundle.set_text(&state.preferences.active.bundle);
        self.echo.set_value(
            f64::from(state.preferences.active.echo_reduction) * f64::from(PERCENT_SCALE),
        );
        self.bass
            .set_value(f64::from(state.preferences.active.bass_db));
        self.latency
            .set_value(state.preferences.active.latency_ms.into());
        self.strength
            .set_value(f64::from(state.preferences.active.strength) * f64::from(PERCENT_SCALE));
        self.updating.set(false);
    }

    fn update(&self, state: &Desktop) {
        let language = state.preferences.language;
        if self.language.get() != Some(language) {
            self.localize(language);
            self.language.set(Some(language));
        }
        if self.revision.get() != state.revision {
            self.sync_choices(state);
            self.revision.set(state.revision);
        }
        let live = state.live.as_ref();
        let ready = live.is_some_and(|l| l.route_ready && l.compute_ready && !l.stopping);
        self.header.set_subtitle(Some(match live {
            Some(l) if l.stopping => language.text(Text::Stopping),
            Some(_) if ready => language.text(Text::Processing),
            Some(_) => language.text(Text::Starting),
            None => language.text(Text::Stopped),
        }));
        self.start.set_label(if live.is_some() {
            language.text(Text::Stop)
        } else {
            language.text(Text::Start)
        });
        self.start
            .set_sensitive(state.picker.is_none() && live.is_none_or(|l| !l.stopping));
        self.browse
            .set_label(language.text(if state.picker.is_some() {
                Text::Cancel
            } else {
                Text::Browse
            }));
        self.bundle.set_editable(state.picker.is_none());
        self.source.set_sensitive(live.is_none());
        self.refresh.set_sensitive(live.is_none());
        self.bundle.set_sensitive(live.is_none());
        self.browse.set_sensitive(live.is_none());
        self.installer
            .set_sensitive(live.is_none() && state.picker.is_none());
        self.adapter.set_sensitive(live.is_none());
        self.latency.set_sensitive(live.is_none());
        self.use_default.set_sensitive(ready);
        self.bass_value
            .set_text(&format!("{:+.1} dB", state.preferences.active.bass_db));
        self.echo_percent.set_text(&format!(
            "{:.0}%",
            state.preferences.active.echo_reduction * PERCENT_SCALE
        ));
        self.percent.set_text(&format!(
            "{:.0}%",
            state.preferences.active.strength * PERCENT_SCALE
        ));
        let output = if live.is_some() && !state.output_name.is_empty() {
            state.output_name.clone()
        } else {
            state
                .sources
                .iter()
                .find(|n| n.name == state.preferences.active.source)
                .map(|n| format!("{} – Clearwave", n.description))
                .unwrap_or_else(|| language.text(Text::ChooseMicrophone).into())
        };
        self.output.set_text(&output);
        let fresh = state
            .last_levels
            .is_some_and(|at| at.elapsed() < METER_STALE_AFTER);
        {
            let bar = &self.meter;
            let meter = &state.output;
            let rms = if fresh { meter.rms } else { 0.0 };
            let db = 20.0 * rms.max(MIN_METER_AMPLITUDE).log10();
            bar.set_value(f64::from(
                ((db + METER_RANGE_DB) / METER_RANGE_DB).clamp(0.0, 1.0),
            ));
            bar.set_tooltip_text(Some(&if fresh && meter.peak > SIGNAL_AMPLITUDE_THRESHOLD {
                format!(
                    "{} {:.1} dBFS",
                    language.text(Text::Peak),
                    20.0 * meter.peak.log10()
                )
            } else {
                language.text(Text::NoMeasured).into()
            }));
        }
        if let Some((error, message)) = &state.message {
            self.message.set_text(message);
            self.settings_message.set_text(message);
            if *error {
                self.message.style_context().add_class("error");
            } else {
                self.message.style_context().remove_class("error");
            }
            self.message.show();
            self.settings_message.show();
        } else {
            self.message.hide();
            self.settings_message.hide();
        }
    }

    fn connect(&self, state: &Rc<RefCell<Desktop>>) {
        for (item, language) in &self.language_items {
            let weak = Rc::downgrade(state);
            let language = *language;
            item.connect_activate(move |_| {
                if let Some(state) = weak.upgrade() {
                    let mut state = state.borrow_mut();
                    if state.preferences.language != language {
                        state.preferences.language = language;
                        state.message = None;
                        state.revision += 1;
                        state.changed();
                    }
                }
            });
        }
        let weak = Rc::downgrade(state);
        self.window.connect_delete_event(move |_, _| {
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                state.picker.take();
                state.live.take();
                state.save(false);
            }
            gtk::main_quit();
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(state);
        self.start.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                if let Some(live) = &mut state.live {
                    live.stop();
                } else if let Err(e) = state.start() {
                    state.message = Some((true, e.to_string()));
                }
            }
        });
        let weak = Rc::downgrade(state);
        self.refresh.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.borrow().refresh();
            }
        });
        let weak = Rc::downgrade(state);
        self.use_default.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                state.message = Some(match crate::pipewire::make_default() {
                    Ok(()) => (
                        false,
                        state
                            .preferences
                            .language
                            .text(Text::DefaultSelected)
                            .into(),
                    ),
                    Err(e) => (true, e.to_string()),
                });
            }
        });
        let weak = Rc::downgrade(state);
        let updating = self.updating.clone();
        self.source.connect_changed(move |combo| {
            if updating.get() {
                return;
            }
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                state.preferences.active.source =
                    combo.active_id().map(|s| s.to_string()).unwrap_or_default();
                state.changed();
            }
        });
        for (scale, key) in [
            (&self.strength, Text::Removal),
            (&self.echo, Text::EchoReduction),
            (&self.bass, Text::Bass),
        ] {
            let weak = Rc::downgrade(state);
            let updating = self.updating.clone();
            scale.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                if let Some(state) = weak.upgrade() {
                    let mut state = state.borrow_mut();
                    let value = scale.value() as f32;
                    match key {
                        Text::Bass => state.preferences.active.bass_db = value,
                        Text::EchoReduction => {
                            state.preferences.active.echo_reduction = value / PERCENT_SCALE
                        }
                        _ => state.preferences.active.strength = value / PERCENT_SCALE,
                    }
                    if let Some(live) = &state.live
                        && let Err(error) =
                            ProcessingControl::write(&live.control, &state.preferences.active)
                    {
                        state.message = Some((true, error.to_string()));
                    }
                    state.changed();
                }
            });
        }
        let weak = Rc::downgrade(state);
        let updating = self.updating.clone();
        self.bundle.connect_changed(move |entry| {
            if updating.get() {
                return;
            }
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                state.preferences.active.bundle = entry.text().into();
                state.changed();
            }
        });
        for (button, installer) in [(&self.browse, false), (&self.installer, true)] {
            let weak = Rc::downgrade(state);
            button.connect_clicked(move |_| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let mut state = state.borrow_mut();
                if state.picker.take().is_some() {
                    return;
                }
                match crate::folder_picker::FolderPicker::start(
                    PathBuf::from(&state.preferences.active.bundle),
                    state.preferences.language,
                    installer,
                ) {
                    Ok(picker) => {
                        state.picker = Some(picker);
                        state.message = installer.then(|| {
                            (
                                false,
                                state.preferences.language.text(Text::SelectingModel).into(),
                            )
                        });
                    }
                    Err(error) => {
                        state.message = Some((
                            true,
                            format!(
                                "{}: {error}",
                                state.preferences.language.text(Text::PickerError)
                            ),
                        ))
                    }
                }
            });
        }
        let weak = Rc::downgrade(state);
        self.settings.connect_hide(move |_| {
            if let Some(state) = weak.upgrade() {
                state.borrow_mut().picker.take();
            }
        });

        let weak = Rc::downgrade(state);
        let updating = self.updating.clone();
        self.adapter.connect_changed(move |combo| {
            if updating.get() {
                return;
            }
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                state.preferences.active.adapter = combo
                    .active_id()
                    .filter(|s| s != "auto")
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                state.changed();
            }
        });
        let weak = Rc::downgrade(state);
        let updating = self.updating.clone();
        self.latency.connect_value_changed(move |spin| {
            if updating.get() {
                return;
            }
            if let Some(state) = weak.upgrade() {
                let mut state = state.borrow_mut();
                state.preferences.active.latency_ms = spin.value_as_int() as u16;
                state.changed();
            }
        });
        let weak = Rc::downgrade(state);
        self.save.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.borrow_mut().save(true);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an explicit preview output directory and a GTK display"]
    fn preview_slider_alignment_and_bass() {
        use gtk::gdk::prelude::*;
        gtk::init().unwrap();
        let ui = Interface::new().unwrap();
        ui.localize(Language::EnglishBritish);
        ui.source.append(Some("dummy"), "USB Microphone");
        ui.source.set_active(Some(0));
        ui.output.set_text("USB Microphone – Clearwave");
        ui.strength.set_value(75.0);
        ui.percent.set_text("75%");
        ui.echo.set_value(20.0);
        ui.echo_percent.set_text("20%");
        ui.bass
            .set_value(f64::from(crate::voice_balance::DEFAULT_BASS_DB));
        ui.bass_value.set_text("+2.5 dB");
        ui.window.show_all();
        ui.message.hide();
        for _ in 0..30 {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for scale in [&ui.strength, &ui.echo, &ui.bass] {
            let range = scale.range_rect();
            let (start, end) = scale.slider_range();
            let panel = scale.parent().unwrap().downcast::<gtk::Box>().unwrap();
            let heading = panel.children()[0].clone();
            let expected = scale.allocation().x() + range.x();
            assert_eq!(heading.allocation().x(), expected);
            println!(
                "heading={}, track={expected}, slider={start}..{end}",
                heading.allocation().x()
            );
        }
        let directory = PathBuf::from(std::env::var_os("CLEARWAVE_PREVIEW_DIRECTORY").unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let size = ui.window.allocation();
        ui.window
            .window()
            .unwrap()
            .pixbuf(0, 0, size.width(), size.height())
            .unwrap()
            .savev(directory.join("linux-bass.png"), "png", &[])
            .unwrap();
        ui.window.close();
    }

    #[test]
    fn closing_live_processing_never_waits_for_a_stuck_child_on_the_ui_thread() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; printf 'ready\n'; exec sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = nix::unistd::Pid::from_raw(child.id() as i32);
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");

        let live = Live {
            child: Some(child),
            control: PathBuf::new(),
            stopping: false,
            route_ready: false,
            compute_ready: false,
        };
        let before = Instant::now();
        drop(live);
        assert!(before.elapsed() < Duration::from_millis(200));

        let deadline = Instant::now() + Duration::from_secs(4);
        while nix::sys::signal::kill(pid, None) != Err(nix::errno::Errno::ESRCH) {
            assert!(Instant::now() < deadline, "Owned child was not reaped");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
