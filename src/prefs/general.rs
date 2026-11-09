use super::*;
use crate::settings::Durability;
use among::notifications::{
    commands::Configuration as Command,
    preferences::{self, AudioSource, Edit, Preferences},
    Eligibility, Policy,
};
use std::cell::RefCell;

type Selected = Box<dyn FnOnce(Result<Durability, String>)>;
type Select = dyn Fn(AudioSource, Selected);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioState {
    Ready,
    Active,
    Reading,
    Retiring,
    Unconfirmed,
    Unavailable,
}

/// What the sound row has to say, as the application records it.
///
/// A problem replaces the sound name with one line. A note is only a tooltip,
/// and nothing is shown after a success, so a working sound stays quiet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AudioReport {
    Idle,
    Info(String),
    Problem(String),
}

impl AudioReport {
    /// The text of a note or problem; empty when there is nothing to say.
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Idle => "",
            Self::Info(text) | Self::Problem(text) => text,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Context {
    pub query: Rc<dyn Fn() -> Preferences>,
    pub edit: Rc<dyn Fn(Edit) -> Result<Durability, String>>,
    pub select: Rc<Select>,
    pub preview: Rc<dyn Fn() -> Result<(), String>>,
    pub cancel_audio: Rc<dyn Fn()>,
    pub audio_state: Rc<dyn Fn() -> AudioState>,
    pub audio_report: Rc<dyn Fn() -> AudioReport>,
    pub desktop_report: Rc<dyn Fn() -> Option<String>>,
    pub providers: Rc<dyn Fn() -> Vec<among::plugin_notifications::ChannelView>>,
    pub roles: Rc<dyn Fn() -> Vec<crate::plugin::host::Overview>>,
    pub review_role: Rc<EnablePlugin>,
}

type ChooseWav = dyn Fn(String);

thread_local! {
    /// The handler a chosen WAV path reaches. The file dialog calls it, and so
    /// does the verification script step, so both exercise the same selection.
    static CHOOSE_WAV: RefCell<Option<std::rc::Weak<ChooseWav>>> = const { RefCell::new(None) };
}

/// Select a local WAV as if the file dialog had returned `path`. Returns false
/// when no Notifications page is open to receive it.
pub(crate) fn choose_wav(path: &str) -> bool {
    let handler = CHOOSE_WAV.with(|slot| slot.borrow().as_ref().and_then(std::rc::Weak::upgrade));
    handler.map(|handler| handler(path.to_owned())).is_some()
}

fn row(group: &adw::PreferencesGroup, title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(&escape_display(subtitle));
    group.add(&row);
    row
}

/// Show `text` in the page banner.
fn announce(banner: &adw::Banner, text: &str) {
    banner.set_title(&escape_display(text));
    banner.set_revealed(true);
}

/// Report the result of a save in the banner: a durable save shows nothing.
fn report(banner: &adw::Banner, result: Result<Durability, String>) {
    match result {
        Ok(Durability::Durable) => banner.set_revealed(false),
        Ok(other) => announce(banner, other.description()),
        Err(error) => announce(banner, &format!("Not saved: {error}")),
    }
}

/// Mark an entry as holding an invalid value: the message replaces its title
/// and the row turns red until the text changes. `None` restores the title.
fn flag(entry: &adw::EntryRow, title: &str, problem: Option<&str>) {
    match problem {
        Some(message) => {
            entry.set_title(&escape_display(message));
            entry.add_css_class("error");
        }
        None => {
            entry.set_title(title);
            entry.remove_css_class("error");
        }
    }
}

fn sound_name(source: &AudioSource) -> String {
    match source {
        AudioSource::Builtin => "Among alert (built-in)".into(),
        AudioSource::Local(path) => std::path::Path::new(path)
            .file_name()
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned()),
    }
}

/// A small icon-only button: same size as its neighbours, named for assistive
/// technology and explained by a tooltip.
fn icon_button(icon: &str, label: &str, name: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_widget_name(name);
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn wav_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("WAV audio"));
    filter.add_mime_type("audio/x-wav");
    filter.add_mime_type("audio/wav");
    filter.add_suffix("wav");
    filter
}

/// Ask for one file and hand its path to `chosen`. A dismissed dialog does
/// nothing.
fn pick_file(
    anchor: &impl IsA<gtk::Widget>,
    title: &str,
    filter: Option<gtk::FileFilter>,
    chosen: impl Fn(String) + 'static,
) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(title);
    if let Some(filter) = filter {
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));
    }
    let window = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    dialog.open(window.as_ref(), None::<&gio::Cancellable>, move |result| {
        if let Some(path) = result.ok().and_then(|file| file.path()) {
            chosen(path.to_string_lossy().into_owned());
        }
    });
}

pub(super) fn page(ctx: Option<&Context>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_name(Some("notifications"));
    page.set_title("Notifications");
    page.set_icon_name(Some("preferences-system-notifications-symbolic"));
    let Some(ctx) = ctx else {
        let group = adw::PreferencesGroup::new();
        row(
            &group,
            "Notifications unavailable",
            "No notification session is attached to this window",
        );
        page.add(&group);
        return page;
    };
    // A save result is shown only when something went wrong; a durable save
    // leaves the page unchanged.
    let banner = adw::Banner::new("");
    banner.set_widget_name("notification-save-status");
    let banner_group = adw::PreferencesGroup::new();
    banner_group.set_visible(false);
    banner_group.add(&banner);
    banner.connect_revealed_notify({
        let group = banner_group.downgrade();
        move |banner| {
            if let Some(group) = group.upgrade() {
                group.set_visible(banner.is_revealed());
            }
        }
    });
    page.add(&banner_group);
    let current = (ctx.query)();

    let visual = adw::PreferencesGroup::new();
    visual.set_title("Visual");
    policy(
        &visual,
        "Highlight the terminal",
        "visual",
        current.visual,
        ctx,
        &banner,
        Edit::Visual,
    );
    page.add(&visual);

    let audio = adw::PreferencesGroup::new();
    audio.set_title("Sound");
    policy(
        &audio,
        "Play a sound",
        "audio",
        current.audio,
        ctx,
        &banner,
        Edit::Audio,
    );
    let (sound_widget, sound) = sound_row(ctx, &banner);
    audio.add(&sound_widget);
    page.add(&audio);

    let desktop = adw::PreferencesGroup::new();
    desktop.set_title("Desktop");
    let desktop_row = policy(
        &desktop,
        "Desktop notification",
        "desktop",
        current.desktop,
        ctx,
        &banner,
        Edit::Desktop,
    );
    desktop_row.set_subtitle(&escape_display(&(ctx.desktop_report)().unwrap_or_default()));
    page.add(&desktop);

    let commands = adw::PreferencesGroup::new();
    commands.set_title("Command");
    let save = gtk::Button::with_label("Save");
    save.set_widget_name("notification-command-save");
    save.add_css_class("flat");
    save.set_valign(gtk::Align::Center);
    commands.set_header_suffix(Some(&save));
    let enabled = policy(
        &commands,
        "Run a command",
        "command",
        current.command,
        ctx,
        &banner,
        Edit::CommandPolicy,
    );
    enabled.set_sensitive(current.configuration.is_some());
    let program = adw::EntryRow::new();
    program.set_title(PROGRAM_TITLE);
    program.set_widget_name("notification-command-program");
    program.set_text(
        current
            .configuration
            .as_ref()
            .map_or("", Command::executable),
    );
    let choose_program = icon_button(
        "document-open-symbolic",
        "Choose the program",
        "notification-command-choose",
    );
    program.add_suffix(&choose_program);
    choose_program.connect_clicked({
        let program = program.downgrade();
        move |button| {
            let program = program.clone();
            pick_file(button, "Choose the program", None, move |path| {
                if let Some(program) = program.upgrade() {
                    program.set_text(&path);
                }
            });
        }
    });
    commands.add(&program);
    let arguments = ArgumentsField::new(
        current
            .configuration
            .as_ref()
            .map_or(&[][..], Command::arguments),
    );
    commands.add(&arguments.row);
    program.connect_changed(|entry| flag(entry, PROGRAM_TITLE, None));
    save.connect_clicked({
        let ctx = ctx.clone();
        let banner = banner.clone();
        let program = program.clone();
        let arguments = arguments.clone();
        move |_| {
            // An empty program clears the configuration and switches the
            // command off; saving never runs it.
            let configuration = if program.text().is_empty() {
                Ok(None)
            } else {
                arguments.parse().and_then(|argv| {
                    Command::new(std::path::Path::new(program.text().as_str()), argv)
                        .map(Some)
                        .map_err(|error| match error {
                            // Each problem is reported on the field that holds it.
                            among::notifications::commands::Error::InvalidArguments => arguments
                                .problem(
                                    "Too many or too long arguments, or one with a NUL character",
                                ),
                            _ => flag(
                                &program,
                                PROGRAM_TITLE,
                                Some("Use an absolute path of at most 16384 bytes"),
                            ),
                        })
                })
            };
            // An invalid value is reported on its own row, so nothing is sent.
            let Ok(configuration) = configuration else {
                return;
            };
            let outcome = (ctx.edit)(Edit::CommandConfiguration(configuration));
            if outcome.is_ok() {
                let accepted = (ctx.query)();
                enabled.set_sensitive(accepted.configuration.is_some());
                if enabled.is_active() != accepted.command.enabled {
                    enabled.set_active(accepted.command.enabled);
                }
            }
            report(&banner, outcome);
        }
    });
    page.add(&commands);

    let providers = adw::PreferencesGroup::new();
    providers.set_title("Plugin channels");
    providers.set_visible(false);
    let provider_rows = Rc::new(RefCell::new(std::collections::BTreeMap::new()));
    provider_rows_refresh(&providers, ctx, &banner, &provider_rows);
    page.add(&providers);

    let providers = providers.downgrade();
    let provider_context = ctx.clone();
    let desktop_row = desktop_row.downgrade();
    let banner = banner.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        let Some(banner) = banner.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !sound.refresh() {
            return glib::ControlFlow::Break;
        }
        if let Some(group) = providers.upgrade() {
            provider_rows_refresh(&group, &provider_context, &banner, &provider_rows);
        }
        if let Some(row) = desktop_row.upgrade() {
            row.set_subtitle(&escape_display(
                &(provider_context.desktop_report)().unwrap_or_default(),
            ));
        }
        glib::ControlFlow::Continue
    });
    page
}

const PROGRAM_TITLE: &str = "Program";

const ARGUMENTS_HINT: &str = "One argument per line";
const ARGUMENTS_JSON_HINT: &str =
    "These arguments cannot be written one per line; edit them as a JSON list";

/// The command's arguments: a multi-line field, one argument per line, so an
/// argument may hold spaces and quotes without any quoting rules.
///
/// Arguments saved earlier that a line cannot hold exactly (an empty one, or
/// one with a line break) are shown and edited as the JSON list they were saved
/// as, so loading them never changes them.
#[derive(Clone)]
struct ArgumentsField {
    row: adw::PreferencesRow,
    view: gtk::TextView,
    hint: gtk::Label,
    json: bool,
}

impl ArgumentsField {
    fn new(initial: &[String]) -> Self {
        let (text, json) = match command_arguments::to_lines(initial) {
            Some(text) => (text, false),
            None => (serde_json::json!(initial).to_string(), true),
        };
        let row = adw::PreferencesRow::new();
        row.set_title("Arguments");
        row.set_activatable(false);
        let hint = gtk::Label::new(Some(if json {
            ARGUMENTS_JSON_HINT
        } else {
            ARGUMENTS_HINT
        }));
        hint.add_css_class("caption");
        hint.add_css_class("dim-label");
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        let title = gtk::Label::new(Some("Arguments"));
        title.set_xalign(0.0);
        let view = gtk::TextView::new();
        view.set_widget_name("notification-command-arguments");
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::None);
        view.set_accepts_tab(false);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        view.set_left_margin(8);
        view.set_right_margin(8);
        view.update_property(&[gtk::accessible::Property::Label(if json {
            "Arguments, as a JSON list"
        } else {
            "Arguments, one per line"
        })]);
        view.buffer().set_text(&text);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&view));
        scroll.set_min_content_height(76);
        scroll.set_max_content_height(180);
        scroll.set_propagate_natural_height(true);
        scroll.set_has_frame(true);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        column.set_margin_top(10);
        column.set_margin_bottom(10);
        column.set_margin_start(12);
        column.set_margin_end(12);
        column.append(&title);
        column.append(&hint);
        column.append(&scroll);
        row.set_child(Some(&column));
        let field = Self {
            row,
            view,
            hint,
            json,
        };
        field
            .view
            .update_relation(&[gtk::accessible::Relation::DescribedBy(&[field
                .hint
                .upcast_ref()])]);
        field.view.buffer().connect_changed({
            let hint = field.hint.downgrade();
            let view = field.view.downgrade();
            move |_| {
                if let Some(view) = view.upgrade() {
                    view.update_state(&[gtk::accessible::State::Invalid(
                        gtk::AccessibleInvalidState::False,
                    )]);
                }
                if let Some(hint) = hint.upgrade() {
                    hint.remove_css_class("error");
                    hint.set_text(if json {
                        ARGUMENTS_JSON_HINT
                    } else {
                        ARGUMENTS_HINT
                    });
                }
            }
        });
        field
    }

    fn text(&self) -> String {
        let buffer = self.view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    /// The arguments as written. A list that cannot be read is reported on the
    /// field and gives `Err`.
    fn parse(&self) -> Result<Vec<String>, ()> {
        let text = self.text();
        if !self.json {
            return Ok(command_arguments::from_lines(&text));
        }
        serde_json::from_str::<Vec<String>>(&text)
            .map_err(|_| self.problem("Arguments must be a JSON list of strings"))
    }

    /// Say what is wrong with the arguments, on the field itself.
    fn problem(&self, text: &str) {
        self.hint.set_text(text);
        self.hint.add_css_class("error");
        // Assistive technology reads the hint as the editor's description and
        // learns that the value is invalid, not only that a label changed.
        self.view.update_state(&[gtk::accessible::State::Invalid(
            gtk::AccessibleInvalidState::True,
        )]);
    }
}

/// The sound row's controls. Only weak references are kept, so the page's
/// callbacks and timer never keep a closed window alive.
#[derive(Clone)]
struct SoundRow {
    row: glib::WeakRef<adw::ActionRow>,
    reset: glib::WeakRef<gtk::Button>,
    play: glib::WeakRef<gtk::Button>,
    spinner: glib::WeakRef<gtk::Spinner>,
    /// Warning mark in front of a problem line, so the problem is not carried
    /// by colour alone.
    problem: glib::WeakRef<gtk::Image>,
    ctx: Context,
}

impl SoundRow {
    /// Bring the row in line with the current sound, state and report.
    /// Returns false once the row is gone.
    fn refresh(&self) -> bool {
        let (Some(row), Some(reset), Some(play), Some(spinner), Some(mark)) = (
            self.row.upgrade(),
            self.reset.upgrade(),
            self.play.upgrade(),
            self.spinner.upgrade(),
            self.problem.upgrade(),
        ) else {
            return false;
        };
        let state = (self.ctx.audio_state)();
        let report = (self.ctx.audio_report)();
        let source = (self.ctx.query)().sound;
        reset.set_visible(matches!(source, AudioSource::Local(_)));
        spinner.set_visible(state == AudioState::Reading);
        spinner.set_spinning(state == AudioState::Reading);
        let (icon, label, enabled) = match state {
            AudioState::Ready => ("media-playback-start-symbolic", "Play the sound", true),
            AudioState::Retiring => (
                "media-playback-start-symbolic",
                "Play the sound; the previous one is still stopping",
                true,
            ),
            AudioState::Active => ("media-playback-stop-symbolic", "Stop the sound", true),
            AudioState::Reading => (
                "media-playback-stop-symbolic",
                "Stop checking the file",
                true,
            ),
            AudioState::Unconfirmed => (
                "media-playback-start-symbolic",
                "Playback is off: the previous sound could not be confirmed stopped",
                false,
            ),
            AudioState::Unavailable => (
                "media-playback-start-symbolic",
                "No sound is ready to play",
                false,
            ),
        };
        play.set_icon_name(icon);
        play.set_tooltip_text(Some(label));
        play.update_property(&[gtk::accessible::Property::Label(label)]);
        play.set_sensitive(enabled);
        if state == AudioState::Active {
            play.add_css_class("suggested-action");
        } else {
            play.remove_css_class("suggested-action");
        }
        match (&report, state) {
            (_, AudioState::Reading) => {
                row.set_subtitle("Checking the file…");
                mark.set_visible(false);
                row.set_tooltip_text(None);
            }
            (AudioReport::Problem(text), _) => {
                row.set_subtitle(&escape_display(text));
                mark.set_visible(true);
                row.set_tooltip_text(None);
            }
            (note, _) => {
                row.set_subtitle(&escape_display(&sound_name(&source)));
                mark.set_visible(false);
                let tooltip = match (&source, note.text()) {
                    (AudioSource::Local(path), "") => Some(path.clone()),
                    (_, "") => None,
                    (_, text) => Some(text.to_owned()),
                };
                row.set_tooltip_text(tooltip.as_deref());
            }
        }
        true
    }
}

fn sound_row(ctx: &Context, banner: &adw::Banner) -> (adw::ActionRow, SoundRow) {
    let row = adw::ActionRow::new();
    row.set_title("Sound");
    row.set_widget_name("notification-audio-source");
    let reset = icon_button(
        "edit-undo-symbolic",
        "Use the built-in sound",
        "notification-audio-builtin",
    );
    let choose = icon_button(
        "document-open-symbolic",
        "Choose a WAV file",
        "notification-audio-select",
    );
    let play = icon_button(
        "media-playback-start-symbolic",
        "Play the sound",
        "notification-audio-preview",
    );
    let spinner = gtk::Spinner::new();
    spinner.set_valign(gtk::Align::Center);
    spinner.set_visible(false);
    spinner.update_property(&[gtk::accessible::Property::Label("Checking the file")]);
    for widget in [
        spinner.upcast_ref::<gtk::Widget>(),
        reset.upcast_ref(),
        choose.upcast_ref(),
        play.upcast_ref(),
    ] {
        row.add_suffix(widget);
    }
    let mark = gtk::Image::from_icon_name("dialog-warning-symbolic");
    mark.set_widget_name("notification-audio-problem");
    mark.add_css_class("error");
    mark.set_visible(false);
    mark.update_property(&[gtk::accessible::Property::Label("Problem with the sound")]);
    row.add_prefix(&mark);
    let sound = SoundRow {
        problem: mark.downgrade(),
        row: row.downgrade(),
        reset: reset.downgrade(),
        play: play.downgrade(),
        spinner: spinner.downgrade(),
        ctx: ctx.clone(),
    };

    // Both the file dialog and the verification step end here. An empty path
    // selects the built-in sound.
    let select: Rc<ChooseWav> = Rc::new({
        let ctx = ctx.clone();
        let banner = banner.downgrade();
        let sound = sound.clone();
        move |text: String| {
            let source = if text.is_empty() {
                AudioSource::Builtin
            } else if !std::path::Path::new(&text).is_absolute()
                || text.len() > 4096
                || text.contains('\0')
            {
                if let Some(banner) = banner.upgrade() {
                    announce(&banner, "Choose a WAV file");
                }
                return;
            } else {
                AudioSource::Local(text)
            };
            let banner = banner.clone();
            let sound = sound.clone();
            (ctx.select)(
                source,
                Box::new(move |outcome| {
                    if let Some(banner) = banner.upgrade() {
                        // A refused file is reported on the row; only a failed
                        // save of an accepted one needs the banner.
                        match outcome {
                            Err(_) => banner.set_revealed(false),
                            ok => report(&banner, ok),
                        }
                    }
                    sound.refresh();
                }),
            );
        }
    });
    CHOOSE_WAV.with(|slot| *slot.borrow_mut() = Some(Rc::downgrade(&select)));
    reset.connect_clicked({
        let select = select.clone();
        move |_| select(String::new())
    });
    choose.connect_clicked({
        let select = select.clone();
        move |button| {
            let select = select.clone();
            pick_file(
                button,
                "Choose a WAV file",
                Some(wav_filter()),
                move |path| select(path),
            );
        }
    });
    play.connect_clicked({
        let ctx = ctx.clone();
        let banner = banner.clone();
        let sound = sound.clone();
        move |_| {
            // One control: it stops while a sound plays or a file is checked,
            // and plays otherwise. Playing works with the channel off.
            match (ctx.audio_state)() {
                AudioState::Active | AudioState::Reading => (ctx.cancel_audio)(),
                _ => {
                    if let Err(error) = (ctx.preview)() {
                        // A busy refusal is already the row's problem line.
                        if (ctx.audio_report)() == AudioReport::Idle {
                            announce(&banner, &error);
                        }
                    }
                }
            }
            sound.refresh();
        }
    });
    (row, sound)
}

/// Opens the existing consent review from the installed plugin's page. The
/// role inventory excludes plugins that declare neither notification role.
pub(super) fn role_row(
    ctx: &Context,
    key: &str,
    is_current: Rc<dyn Fn() -> bool>,
) -> Option<adw::ActionRow> {
    let entry = (ctx.roles)().into_iter().find(|entry| entry.key == key)?;
    let row = adw::ActionRow::new();
    row.set_title("Notification role");
    row.set_subtitle_lines(0);
    row.set_widget_name(&format!("notification-role-status:{}", entry.key));
    let review = gtk::Button::with_label("Review role / access");
    review.set_widget_name(&format!("notification-review-role:{}", entry.key));
    review.update_property(&[gtk::accessible::Property::Label(&format!(
        "Review notification role and access for {}",
        entry.key
    ))]);
    review.set_valign(gtk::Align::Center);
    review.connect_clicked({
        let callback = ctx.review_role.clone();
        let key = entry.key.clone();
        move |button| {
            if !is_current() {
                return;
            }
            callback(
                &key,
                button
                    .root()
                    .and_then(|root| root.downcast::<gtk::Window>().ok()),
            )
        }
    });
    row.add_suffix(&review);
    row.set_activatable_widget(Some(&review));
    row.set_subtitle(&escape_display(&format!(
        "{} · {}",
        entry.name, entry.declared
    )));
    Some(row)
}

fn provider_rows_refresh(
    group: &adw::PreferencesGroup,
    ctx: &Context,
    result: &adw::Banner,
    rows: &RefCell<
        std::collections::BTreeMap<among::notifications::providers::Key, adw::SwitchRow>,
    >,
) {
    // The group exists only while there is a channel to configure: an installed
    // plugin's, or a retained choice of one that is gone, which stays editable.
    group.set_visible(!(ctx.query)().providers.is_empty());
    let inventory: std::collections::BTreeMap<_, _> = (ctx.providers)()
        .into_iter()
        .filter_map(|channel| {
            let key = &channel.declaration.key;
            among::notifications::providers::Key::new(&key.identity, &key.channel)
                .ok()
                .map(|key| (key, channel))
        })
        .collect();
    for (key, value) in (ctx.query)().providers {
        let mut rows = rows.borrow_mut();
        let enabled = rows.entry(key.clone()).or_insert_with(|| {
            let captured = key.clone();
            policy(
                group,
                &format!("{} · {}", key.identity, key.channel),
                &format!("provider:{}:{}", key.identity, key.channel),
                value,
                ctx,
                result,
                move |policy| Edit::Provider(captured.clone(), policy),
            )
        });
        let channel = inventory.get(&key);
        let subtitle = match channel {
            Some(channel) if channel.installed => format!(
                "{} — {}. {}",
                channel.declaration.name,
                channel.declaration.description,
                if channel.owner.is_some() {
                    "Admitted provider process"
                } else if channel.plugin_enabled {
                    "Waiting for process admission"
                } else {
                    "Plugin disabled; channel permission does not start it"
                }
            ),
            _ => "Unavailable; choice retained, no delivery consumer is attached".into(),
        };
        enabled.set_subtitle(&escape_display(&subtitle));
    }
}

fn policy(
    group: &adw::PreferencesGroup,
    title: &str,
    identity: &str,
    initial: Policy,
    ctx: &Context,
    result: &adw::Banner,
    edit: impl Fn(Policy) -> Edit + 'static,
) -> adw::SwitchRow {
    let enabled = adw::SwitchRow::new();
    enabled.set_title(&escape_display(title));
    enabled.set_widget_name(&if identity == "desktop" {
        "notification-desktop-switch".into()
    } else {
        format!("notification-{identity}-enabled")
    });
    enabled.set_active(initial.enabled);
    group.add(&enabled);
    let condition = adw::ComboRow::new();
    condition.set_title("When");
    // Every group has a row called "When"; assistive technology hears which one.
    condition
        .upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(&format!("{title}: when"))]);
    condition.set_widget_name(&if identity == "desktop" {
        "notification-desktop-when".into()
    } else {
        format!("notification-{identity}-condition")
    });
    // The stored value is originating-terminal-unfocused: it also applies while
    // Among is in the background, which the label cannot say in a few words.
    let explanation = if identity == "desktop" {
        "Desktop notifications never alert for the terminal you are viewing. Always has the same effect as Terminal not in view, including while Among is in the background."
    } else {
        "\"Terminal not in view\" also applies while Among is in the background."
    };
    condition.set_tooltip_text(Some(explanation));
    if identity == "desktop" {
        condition
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Description(explanation)]);
    }
    condition.set_model(Some(&gtk::StringList::new(&[
        "Always",
        "Terminal not in view",
        "Among in background",
    ])));
    condition.set_selected(match initial.eligibility {
        Eligibility::Always => 0,
        Eligibility::OriginatingTerminalUnfocused => 1,
        Eligibility::ApplicationBackground => 2,
    });
    group.add(&condition);
    let apply: Rc<dyn Fn()> = Rc::new({
        let ctx = ctx.clone();
        let enabled = enabled.downgrade();
        let condition = condition.downgrade();
        let result = result.downgrade();
        move || {
            let (Some(enabled), Some(condition), Some(result)) =
                (enabled.upgrade(), condition.upgrade(), result.upgrade())
            else {
                return;
            };
            let Some(value) = preferences::CONDITIONS.get(condition.selected() as usize) else {
                return;
            };
            let Ok(eligibility) = preferences::eligibility(value) else {
                return;
            };
            report(
                &result,
                (ctx.edit)(edit(Policy {
                    enabled: enabled.is_active(),
                    eligibility,
                })),
            );
        }
    });
    enabled.connect_active_notify({
        let apply = apply.clone();
        move |_| apply()
    });
    condition.connect_selected_notify(move |_| apply());
    enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_sound_is_named_by_its_file_not_its_directory() {
        assert_eq!(sound_name(&AudioSource::Builtin), "Among alert (built-in)");
        assert_eq!(
            sound_name(&AudioSource::Local("/home/me/sounds/ding.wav".into())),
            "ding.wav"
        );
        // A path without a final component still reads as something.
        assert_eq!(sound_name(&AudioSource::Local("/".into())), "/");
    }

    #[test]
    fn only_a_problem_or_a_note_carries_text() {
        assert_eq!(AudioReport::Idle.text(), "");
        assert_eq!(AudioReport::Info("note".into()).text(), "note");
        assert_eq!(AudioReport::Problem("bad".into()).text(), "bad");
    }

    /// A context whose audio state and report the test controls.
    fn context(
        state: Rc<Cell<AudioState>>,
        report: Rc<RefCell<AudioReport>>,
        sound: Rc<RefCell<AudioSource>>,
        previews: Rc<Cell<u32>>,
        cancels: Rc<Cell<u32>>,
    ) -> Context {
        Context {
            query: Rc::new(move || Preferences {
                sound: sound.borrow().clone(),
                ..Preferences::default()
            }),
            edit: Rc::new(|_| Ok(Durability::Durable)),
            select: Rc::new(|_, finished| finished(Ok(Durability::Durable))),
            preview: Rc::new(move || {
                previews.set(previews.get() + 1);
                Ok(())
            }),
            cancel_audio: Rc::new(move || cancels.set(cancels.get() + 1)),
            audio_state: Rc::new(move || state.get()),
            audio_report: Rc::new(move || report.borrow().clone()),
            desktop_report: Rc::new(|| None),
            providers: Rc::new(Vec::new),
            roles: Rc::new(Vec::new),
            review_role: Rc::new(|_, _| {}),
        }
    }

    fn find<T: IsA<gtk::Widget>>(widget: &gtk::Widget, name: &str) -> T {
        fn walk(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
            if widget.widget_name() == name {
                return Some(widget.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(found) = walk(&current, name) {
                    return Some(found);
                }
                child = current.next_sibling();
            }
            None
        }
        walk(widget, name)
            .unwrap_or_else(|| panic!("missing control {name}"))
            .downcast::<T>()
            .unwrap_or_else(|_| panic!("control {name} has another type"))
    }

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn the_sound_row_has_one_slot_that_plays_stops_or_is_disabled_with_a_reason() {
        adw::init().expect("isolated GTK display");
        let state = Rc::new(Cell::new(AudioState::Ready));
        let report = Rc::new(RefCell::new(AudioReport::Idle));
        let sound = Rc::new(RefCell::new(AudioSource::Builtin));
        let (previews, cancels) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let page = page(Some(&context(
            state.clone(),
            report.clone(),
            sound.clone(),
            previews.clone(),
            cancels.clone(),
        )));
        let root: &gtk::Widget = page.upcast_ref();
        let play: gtk::Button = find(root, "notification-audio-preview");
        let reset: gtk::Button = find(root, "notification-audio-builtin");
        let row: adw::ActionRow = find(root, "notification-audio-source");
        let problem: gtk::Image = find(root, "notification-audio-problem");
        let pump = || {
            for _ in 0..30 {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };

        // Idle with the built-in sound: Play works, nothing to reset, no problem.
        pump();
        assert_eq!(
            play.icon_name().as_deref(),
            Some("media-playback-start-symbolic")
        );
        assert!(play.is_sensitive() && !reset.is_visible() && !problem.is_visible());
        assert_eq!(row.subtitle().as_deref(), Some("Among alert (built-in)"));
        play.emit_clicked();
        assert_eq!((previews.get(), cancels.get()), (1, 0));

        // While a sound plays the same slot stops it instead of previewing.
        state.set(AudioState::Active);
        pump();
        assert_eq!(
            play.icon_name().as_deref(),
            Some("media-playback-stop-symbolic")
        );
        play.emit_clicked();
        assert_eq!((previews.get(), cancels.get()), (1, 1));

        // Checking a chosen file shows Stop, which cancels the check.
        state.set(AudioState::Reading);
        pump();
        assert_eq!(
            play.icon_name().as_deref(),
            Some("media-playback-stop-symbolic")
        );
        assert_eq!(row.subtitle().as_deref(), Some("Checking the file…"));
        play.emit_clicked();
        assert_eq!(cancels.get(), 2);

        // A custom file shows its name and the reset control.
        state.set(AudioState::Ready);
        *sound.borrow_mut() = AudioSource::Local("/music/ding.wav".into());
        pump();
        assert!(reset.is_visible());
        assert_eq!(row.subtitle().as_deref(), Some("ding.wav"));

        // A problem replaces the name with one line and marks it.
        *report.borrow_mut() = AudioReport::Problem("Not a WAV file".into());
        pump();
        assert_eq!(row.subtitle().as_deref(), Some("Not a WAV file"));
        assert!(problem.is_visible());

        // Without a confirmed decoder the slot is disabled and says why.
        *report.borrow_mut() = AudioReport::Idle;
        state.set(AudioState::Unconfirmed);
        pump();
        assert!(!play.is_sensitive());
        assert!(play
            .tooltip_text()
            .is_some_and(|text| text.contains("could not be confirmed")));
    }

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn the_page_has_short_labels_and_no_explanatory_plates() {
        adw::init().expect("isolated GTK display");
        let page = page(Some(&context(
            Rc::new(Cell::new(AudioState::Ready)),
            Rc::new(RefCell::new(AudioReport::Idle)),
            Rc::new(RefCell::new(AudioSource::Builtin)),
            Rc::new(Cell::new(0)),
            Rc::new(Cell::new(0)),
        )));
        let root: &gtk::Widget = page.upcast_ref();
        for identity in ["visual", "audio", "command"] {
            let condition: adw::ComboRow =
                find(root, &format!("notification-{identity}-condition"));
            let labels: Vec<String> = (0..3)
                .map(|index| {
                    condition
                        .model()
                        .and_downcast::<gtk::StringList>()
                        .and_then(|list| list.string(index))
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            assert_eq!(
                labels,
                ["Always", "Terminal not in view", "Among in background"]
            );
            assert_eq!(condition.title(), "When");
        }
        // The save banner stays hidden until a save fails.
        let banner: adw::Banner = find(root, "notification-save-status");
        assert!(!banner.is_revealed());
        // The typed path field and its buttons are gone.
        for removed in ["notification-audio-path", "notification-audio-cancel"] {
            let mut found = false;
            let mut stack = vec![root.clone()];
            while let Some(widget) = stack.pop() {
                found |= widget.widget_name() == removed;
                let mut child = widget.first_child();
                while let Some(current) = child {
                    child = current.next_sibling();
                    stack.push(current);
                }
            }
            assert!(!found, "{removed} should no longer exist");
        }
    }
}

#[cfg(test)]
mod arguments_tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn the_arguments_field_shows_one_per_line_and_reads_them_back_exactly() {
        adw::init().expect("isolated GTK display");
        let saved = list(&["--title", "a b", "\"quoted\"", "; $HOME"]);
        let field = ArgumentsField::new(&saved);
        assert_eq!(field.text(), "--title\na b\n\"quoted\"\n; $HOME");
        assert_eq!(field.parse(), Ok(saved));
        // Editing with blank lines in between changes nothing but the blanks.
        field.view.buffer().set_text("x\n\n  y  \n\n");
        assert_eq!(field.parse(), Ok(list(&["x", "  y  "])));
        field.view.buffer().set_text("");
        assert_eq!(field.parse(), Ok(Vec::new()));
    }

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn arguments_a_line_cannot_hold_stay_a_json_list_and_load_unchanged() {
        adw::init().expect("isolated GTK display");
        let saved = list(&["a", "", "two\nlines"]);
        let field = ArgumentsField::new(&saved);
        assert!(field.json);
        assert_eq!(field.parse(), Ok(saved));
        field.view.buffer().set_text("not json");
        assert_eq!(field.parse(), Err(()));
        assert!(field.hint.has_css_class("error"));
    }
}
