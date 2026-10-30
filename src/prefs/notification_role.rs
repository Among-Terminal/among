use super::*;
use crate::plugin::{
    data::Problem,
    managed::{self, NotificationRole},
    manifest::Manifest,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Unchosen,
    None,
    Role(NotificationRole),
}

pub(crate) struct Picker {
    pub group: adw::PreferencesGroup,
    pub row: adw::ComboRow,
    choices: Vec<Choice>,
}

pub(crate) fn feature(role: NotificationRole) -> &'static str {
    match role {
        NotificationRole::Producer => "notifications.produce",
        NotificationRole::Delivery => "notifications.deliver",
    }
}

pub(crate) fn is_role(feature: &str) -> bool {
    matches!(feature, "notifications.produce" | "notifications.deliver")
}

pub(crate) fn label(role: Option<NotificationRole>) -> &'static str {
    match role {
        Some(NotificationRole::Producer) => "Producer",
        Some(NotificationRole::Delivery) => "Delivery",
        None => "None",
    }
}

impl Picker {
    pub(crate) fn new(
        manifest: &Manifest,
        reviewed: Result<Option<NotificationRole>, Problem>,
        available: &[&str],
    ) -> Result<Option<Rc<Self>>, Problem> {
        let required = managed::required_notification_role(manifest)
            .map_err(managed::NotificationRoleConflict::problem)?;
        if !manifest
            .required_features
            .iter()
            .chain(&manifest.optional_features)
            .any(|feature| is_role(feature))
        {
            return Ok(None);
        }
        let mut choices = Vec::new();
        let mut labels = Vec::new();
        if let Some(role) = required {
            choices.push(Choice::Role(role));
            labels.push(format!("{} (required)", label(Some(role))));
        } else {
            choices.extend([Choice::Unchosen, Choice::None]);
            labels.extend([
                "Choose a notification role…".into(),
                "Neither producer nor delivery".into(),
            ]);
            for role in [NotificationRole::Producer, NotificationRole::Delivery] {
                let feature = feature(role);
                if manifest
                    .optional_features
                    .iter()
                    .any(|declared| declared == feature)
                    && available.contains(&feature)
                {
                    choices.push(Choice::Role(role));
                    labels.push(label(Some(role)).into());
                }
            }
        }
        let group = adw::PreferencesGroup::new();
        group.set_title("Notification authority for this process");
        group.set_description(Some("Production and delivery are mutually exclusive. UI contributions are independent. Changing this choice does not save, start code, restart a generation or enable a delivery channel."));
        let row = adw::ComboRow::new();
        row.set_title("Notification role");
        row.set_widget_name("notification-role-choice");
        row.upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label(
                "Notification role for this process",
            )]);
        let references: Vec<_> = labels.iter().map(String::as_str).collect();
        row.set_model(Some(&gtk::StringList::new(&references)));
        let selected = if required.is_some() {
            0
        } else {
            reviewed
                .ok()
                .and_then(|role| {
                    choices
                        .iter()
                        .position(|choice| *choice == role.map_or(Choice::None, Choice::Role))
                })
                .unwrap_or(0)
        };
        row.set_selected(selected as u32);
        row.set_sensitive(required.is_none());
        group.add(&row);
        Ok(Some(Rc::new(Self {
            group,
            row,
            choices,
        })))
    }

    pub(crate) fn selected(&self) -> Result<Option<NotificationRole>, Problem> {
        let role = match self.choices.get(self.row.selected() as usize) {
            Some(Choice::None) => None,
            Some(Choice::Role(role)) => Some(*role),
            _ => {
                return Err(Problem(
                    "explicitly choose producer, delivery or neither notification role",
                ))
            }
        };
        let features = role
            .into_iter()
            .map(|role| feature(role).to_owned())
            .collect();
        managed::notification_role(&features).map_err(managed::NotificationRoleConflict::problem)
    }
}
