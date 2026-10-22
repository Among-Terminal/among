use super::*;
use crate::settings::Durability;
use among::notifications::{
    commands::Configuration as Command,
    preferences::{self, AudioSource, Edit, Preferences},
    Eligibility, Policy,
};

type Selected = Box<dyn FnOnce(Result<Durability, String>)>;
type Select = dyn Fn(AudioSource, Selected);

#[derive(Clone)]
pub(crate) struct Context {
    pub query: Rc<dyn Fn() -> Preferences>,
    pub edit: Rc<dyn Fn(Edit) -> Result<Durability, String>>,
    pub select: Rc<Select>,
    pub preview: Rc<dyn Fn() -> Result<(), String>>,
    pub status: Rc<dyn Fn() -> (String, String)>,
    pub provider_available: Rc<dyn Fn(&among::notifications::providers::Key) -> bool>,
}

fn row(group: &adw::PreferencesGroup, title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(&escape_display(subtitle));
    group.add(&row);
    row
}

fn report(row: &adw::ActionRow, result: Result<Durability, String>) {
    row.set_subtitle(&escape_display(match &result {
        Ok(value) => value.description(),
        Err(error) => error,
    }));
}

pub(super) fn page(ctx: Option<&Context>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_name(Some("general"));
    page.set_title("General");
    page.set_icon_name(Some("preferences-system-symbolic"));
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
    let feedback = adw::PreferencesGroup::new();
    feedback.set_title("Notification settings status");
    let result = row(
        &feedback,
        "Local notification changes",
        "No edits made in this window",
    );
    result.set_widget_name("notification-save-status");
    let current = (ctx.query)();
    let visual = adw::PreferencesGroup::new();
    visual.set_title("Visual attention");
    visual.set_description(Some("The originating header and workspace stripe pulse briefly, then retain an unread ! until that terminal is actually focused. System reduced motion is respected."));
    policy(
        &visual,
        "Visual alerts",
        "visual",
        current.visual,
        ctx,
        &result,
        Edit::Visual,
    );
    page.add(&visual);

    let audio = adw::PreferencesGroup::new();
    audio.set_title("Audio");
    audio.set_description(Some("One sound at a time; no queued playback. Preview works while audio events are off and does not create a notification."));
    policy(
        &audio,
        "Audio alerts",
        "audio",
        current.audio,
        ctx,
        &result,
        Edit::Audio,
    );
    let selected = row(&audio, "Accepted sound", &source_label(&current.sound));
    selected.set_widget_name("notification-audio-source");
    let builtin = gtk::Button::with_label("Use built-in WAV");
    builtin.set_widget_name("notification-audio-builtin");
    selected.add_suffix(&builtin);
    let path = adw::EntryRow::new();
    path.set_title("Local WAV absolute path");
    path.set_widget_name("notification-audio-path");
    audio.add(&path);
    let choose = gtk::Button::with_label("Use local WAV");
    choose.set_widget_name("notification-audio-select");
    choose.set_valign(gtk::Align::Center);
    path.add_suffix(&choose);
    let playback = row(
        &audio,
        "Playback status",
        "No output is claimed from a playback request or decoder status",
    );
    let preview = gtk::Button::with_label("Preview");
    preview.set_widget_name("notification-audio-preview");
    preview.update_property(&[gtk::accessible::Property::Label(
        "Preview accepted notification sound, even when audio alerts are off",
    )]);
    preview.set_valign(gtk::Align::Center);
    playback.add_suffix(&preview);
    for (button, local) in [(&builtin, false), (&choose, true)] {
        let ctx = ctx.clone();
        let path = path.clone();
        let selected = selected.downgrade();
        let result = result.downgrade();
        button.connect_clicked(move |_| {
            let source = if local {
                let text = path.text().to_string();
                if !std::path::Path::new(&text).is_absolute()
                    || text.len() > 4096
                    || text.contains('\0')
                {
                    if let Some(result) = result.upgrade() {
                        report(&result, Err("Select a local absolute WAV path".into()));
                    }
                    return;
                }
                AudioSource::Local(text)
            } else {
                AudioSource::Builtin
            };
            if let Some(result) = result.upgrade() {
                result.set_subtitle("Reading and validating WAV off the UI loop…");
            }
            let source_label = source_label(&source);
            let selected = selected.clone();
            let result = result.clone();
            (ctx.select)(
                source,
                Box::new(move |outcome| {
                    if outcome.is_ok() {
                        if let Some(selected) = selected.upgrade() {
                            selected.set_subtitle(&escape_display(&source_label));
                        }
                    }
                    if let Some(result) = result.upgrade() {
                        report(&result, outcome);
                    }
                }),
            );
        });
    }
    preview.connect_clicked({
        let ctx = ctx.clone();
        let result = result.clone();
        move |_| {
            if let Err(error) = (ctx.preview)() {
                report(&result, Err(error));
            }
        }
    });
    page.add(&audio);

    let commands = adw::PreferencesGroup::new();
    commands.set_title("External command");
    commands.set_description(Some("Off and unconfigured by default. Explicit executable plus literal argv; no shell expansion or terminal environment. Commands run with your ordinary user authority, not in an OS sandbox."));
    let enabled = policy(
        &commands,
        "Command alerts",
        "command",
        current.command,
        ctx,
        &result,
        Edit::CommandPolicy,
    );
    enabled.set_sensitive(current.configuration.is_some());
    let program = adw::EntryRow::new();
    program.set_title("Executable absolute path");
    program.set_widget_name("notification-command-program");
    program.set_text(
        current
            .configuration
            .as_ref()
            .map_or("", Command::executable),
    );
    commands.add(&program);
    let arguments = adw::EntryRow::new();
    arguments.set_title("Literal arguments (JSON string array)");
    arguments.set_widget_name("notification-command-arguments");
    arguments.set_text(
        &current
            .configuration
            .as_ref()
            .map(|config| serde_json::json!(config.arguments()).to_string())
            .unwrap_or_else(|| "[]".into()),
    );
    commands.add(&arguments);
    let apply = row(
        &commands,
        "Apply command configuration",
        "An empty executable clears configuration and disables commands. Saving does not run it.",
    );
    let save = gtk::Button::with_label("Save command");
    save.set_widget_name("notification-command-save");
    save.set_valign(gtk::Align::Center);
    apply.add_suffix(&save);
    save.connect_clicked({
        let ctx = ctx.clone();
        let result = result.clone();
        move |_| {
            let configuration = if program.text().is_empty() {
                Ok(None)
            } else {
                serde_json::from_str::<Vec<String>>(&arguments.text())
                    .map_err(|_| "Arguments must be a JSON string array".to_string())
                    .and_then(|argv| {
                        Command::new(std::path::Path::new(program.text().as_str()), argv)
                            .map(Some)
                            .map_err(|_| {
                                "Use an absolute executable and bounded literal arguments".into()
                            })
                    })
            };
            let outcome = configuration
                .and_then(|configuration| (ctx.edit)(Edit::CommandConfiguration(configuration)));
            if outcome.is_ok() {
                let accepted = (ctx.query)();
                enabled.set_sensitive(accepted.configuration.is_some());
                if enabled.is_active() != accepted.command.enabled {
                    enabled.set_active(accepted.command.enabled);
                }
            }
            report(&result, outcome);
        }
    });
    let command_status = row(&commands, "Command runtime", &(ctx.status)().1);
    page.add(&commands);

    let providers = adw::PreferencesGroup::new();
    providers.set_title("Provider channels");
    row(&providers, "Managed notification providers unavailable", "Managed delivery needs the committed provider API integration. Installing or inspecting a plugin does not grant notification permission.");
    for (key, value) in &current.providers {
        let title = format!("{} · {}", key.identity, key.channel);
        let identity = format!("provider:{}:{}", key.identity, key.channel);
        let captured = key.clone();
        let enabled = policy(
            &providers,
            &title,
            &identity,
            *value,
            ctx,
            &result,
            move |policy| Edit::Provider(captured.clone(), policy),
        );
        enabled.set_subtitle(if (ctx.provider_available)(key) {
            "Available admitted channel"
        } else {
            "Unavailable; choice retained, no delivery consumer is attached"
        });
    }
    page.add(&providers);
    page.add(&feedback);
    let status = ctx.status.clone();
    let playback = playback.downgrade();
    let command_status = command_status.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        let (Some(audio), Some(command)) = (playback.upgrade(), command_status.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let (a, c) = status();
        audio.set_subtitle(&escape_display(&a));
        command.set_subtitle(&escape_display(&c));
        glib::ControlFlow::Continue
    });
    page
}

fn source_label(source: &AudioSource) -> String {
    match source {
        AudioSource::Builtin => "Built-in original Among alert WAV".into(),
        AudioSource::Local(path) => path.clone(),
    }
}

fn policy(
    group: &adw::PreferencesGroup,
    title: &str,
    identity: &str,
    initial: Policy,
    ctx: &Context,
    result: &adw::ActionRow,
    edit: impl Fn(Policy) -> Edit + 'static,
) -> adw::SwitchRow {
    let enabled = adw::SwitchRow::new();
    enabled.set_title(&escape_display(title));
    enabled.set_widget_name(&format!("notification-{identity}-enabled"));
    enabled.set_active(initial.enabled);
    group.add(&enabled);
    let condition = adw::ComboRow::new();
    condition.set_title(&escape_display(&format!("{title} condition")));
    condition.set_widget_name(&format!("notification-{identity}-condition"));
    condition.set_model(Some(&gtk::StringList::new(&[
        "Always",
        "Originating terminal unfocused",
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
