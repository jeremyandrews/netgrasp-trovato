//! Netgrasp plugin for Trovato.
//!
//! Network monitoring use case: 6 content types for devices, people, events,
//! presence sessions, IP history, and location tracking.

use trovato_sdk::prelude::*;

/// The 6 Netgrasp content types.
///
/// Field naming: Uses bare field names (e.g., `mac`, `display_name`) rather
/// than the `field_` prefix convention. This matches the data and gather query
/// field references from the original kernel implementation. New plugins should
/// prefer the `field_` prefix (see argus/goose plugins for examples).
///
/// Type change: `owner_id` is now `RecordReference("ng_person")` instead of
/// plain Text. This is safe because the plugin registration path creates fresh
/// content types — there is no existing data to migrate.
///
/// Self-references: `ng_device` references `ng_person` (defined below in the
/// same Vec). This is safe because `sync_from_plugins` registers all types from
/// a single `tap_item_info` response without validating reference targets. The
/// same pattern applies to `ng_event`, `ng_presence`, etc. referencing `ng_device`.
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "ng_device".into(),
            label: "Device".into(),
            description: "Network device tracked by Netgrasp".into(),
            fields: vec![
                FieldDefinition::new("mac", FieldType::Text { max_length: None })
                    .required()
                    .label("MAC Address"),
                FieldDefinition::new("display_name", FieldType::Text { max_length: None })
                    .label("Display Name"),
                FieldDefinition::new("hostname", FieldType::Text { max_length: None })
                    .label("Hostname"),
                FieldDefinition::new("vendor", FieldType::Text { max_length: None })
                    .label("Vendor"),
                FieldDefinition::new("device_type", FieldType::Text { max_length: None })
                    .label("Device Type"),
                FieldDefinition::new("os_family", FieldType::Text { max_length: None })
                    .label("OS Family"),
                FieldDefinition::new("state", FieldType::Text { max_length: None }).label("State"),
                FieldDefinition::new("last_ip", FieldType::Text { max_length: None })
                    .label("Last IP"),
                FieldDefinition::new("current_ap", FieldType::Text { max_length: None })
                    .label("Current AP"),
                FieldDefinition::new("owner_id", FieldType::RecordReference("ng_person".into()))
                    .label("Owner"),
                FieldDefinition::new("hidden", FieldType::Boolean).label("Hidden"),
                FieldDefinition::new("notify", FieldType::Boolean).label("Notify"),
                FieldDefinition::new("baseline", FieldType::Boolean).label("Baseline"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "ng_person".into(),
            label: "Person".into(),
            description: "Person associated with network devices".into(),
            fields: vec![
                FieldDefinition::new("name", FieldType::Text { max_length: None })
                    .required()
                    .label("Name"),
                FieldDefinition::new("notes", FieldType::TextLong).label("Notes"),
                FieldDefinition::new("notification_prefs", FieldType::Text { max_length: None })
                    .label("Notification Preferences"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "ng_event".into(),
            label: "Event".into(),
            description: "Network event (device seen, new device, etc.)".into(),
            fields: vec![
                FieldDefinition::new("device_id", FieldType::RecordReference("ng_device".into()))
                    .required()
                    .label("Device"),
                FieldDefinition::new("event_type", FieldType::Text { max_length: None })
                    .required()
                    .label("Event Type"),
                FieldDefinition::new("timestamp", FieldType::Integer)
                    .required()
                    .label("Timestamp"),
                FieldDefinition::new("details", FieldType::TextLong).label("Details"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "ng_presence".into(),
            label: "Presence Session".into(),
            description: "Device presence session (online period)".into(),
            fields: vec![
                FieldDefinition::new("device_id", FieldType::RecordReference("ng_device".into()))
                    .required()
                    .label("Device"),
                FieldDefinition::new("start_time", FieldType::Integer)
                    .required()
                    .label("Start Time"),
                FieldDefinition::new("end_time", FieldType::Integer).label("End Time"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "ng_ip_history".into(),
            label: "IP History".into(),
            description: "Historical IP address assignments for devices".into(),
            fields: vec![
                FieldDefinition::new("device_id", FieldType::RecordReference("ng_device".into()))
                    .required()
                    .label("Device"),
                FieldDefinition::new("ip_address", FieldType::Text { max_length: None })
                    .required()
                    .label("IP Address"),
                FieldDefinition::new("first_seen", FieldType::Integer)
                    .required()
                    .label("First Seen"),
                FieldDefinition::new("last_seen", FieldType::Integer).label("Last Seen"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "ng_location".into(),
            label: "Location History".into(),
            description: "Device location history".into(),
            fields: vec![
                FieldDefinition::new("device_id", FieldType::RecordReference("ng_device".into()))
                    .required()
                    .label("Device"),
                FieldDefinition::new("location", FieldType::Text { max_length: None })
                    .required()
                    .label("Location"),
                FieldDefinition::new("start_time", FieldType::Integer)
                    .required()
                    .label("Start Time"),
                FieldDefinition::new("end_time", FieldType::Integer).label("End Time"),
            ],
        },
    ]
}

const NG_TYPES: &[&str] = &[
    "ng_device",
    "ng_person",
    "ng_event",
    "ng_presence",
    "ng_ip_history",
    "ng_location",
];

/// Permissions: view / create / edit / delete for each of the 6 content types.
///
/// Permission format matches kernel fallback: "{operation} {type} content".
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    NG_TYPES
        .iter()
        .flat_map(|t| PermissionDefinition::crud_for_type(t))
        .collect()
}

/// Menu routes: /devices and /events listings.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/devices", "Devices")
            .callback("ng_device_list")
            .permission("access content"),
        MenuDefinition::new("/events", "Events")
            .callback("ng_event_log")
            .permission("access content"),
    ]
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_info_returns_six_types() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 6);
        let names: Vec<&str> = types.iter().map(|t| t.machine_name.as_str()).collect();
        assert!(names.contains(&"ng_device"));
        assert!(names.contains(&"ng_person"));
        assert!(names.contains(&"ng_event"));
        assert!(names.contains(&"ng_presence"));
        assert!(names.contains(&"ng_ip_history"));
        assert!(names.contains(&"ng_location"));
    }

    #[test]
    fn ng_device_has_thirteen_fields() {
        let types = __inner_tap_item_info();
        let device = types
            .iter()
            .find(|t| t.machine_name == "ng_device")
            .unwrap();
        assert_eq!(device.fields.len(), 13);
    }

    #[test]
    fn perm_returns_twenty_four_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 24); // 4 per type × 6 types (view/create/edit/delete)
    }

    #[test]
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/devices");
        assert_eq!(menus[1].path, "/events");
    }

    #[test]
    fn perm_format_matches_kernel_fallback() {
        let perms = __inner_tap_perm();
        // Kernel fallback: "{operation} {type} content" — no "any" qualifier
        for perm in &perms {
            assert!(
                !perm.name.contains(" any "),
                "permission '{}' must not contain 'any' — kernel fallback uses '{{op}} {{type}} content'",
                perm.name
            );
        }
    }
}
