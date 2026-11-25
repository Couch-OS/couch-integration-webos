//! Hierarchical HDMI-CEC input discovery for LG webOS.
//!
//! Physical inputs remain top-level choices. EIM records identify the
//! physical input with "id", its HDMI launch app with "appId", and expose the
//! CEC descendants through "subList". Duplicate
//! suppression compares depth-one CEC names against the TV's actual top-level
//! input labels, and also uses CEC identity when the TV exposes it. No model or
//! device name is hardcoded.

use couch_sdk::Selectable;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CecSelection {
    pub(crate) parent_input_id: String,
    pub(crate) parent_app_id: String,
    pub(crate) unique_id: String,
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.to_owned())
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn row_input_id(row: &Value) -> Option<&str> {
    row.get("inputId")
        .and_then(Value::as_str)
        .or_else(|| row.get("id").and_then(Value::as_str))
}

fn identity(row: &Value) -> Option<(String, String)> {
    let vendor = scalar(row.get("vendorId")?)?;
    let address = scalar(row.get("physicalAddress")?)?;
    if matches!(address.as_str(), "0" | "0x0" | "0x0000") {
        return None;
    }
    Some((vendor, address))
}

fn child_id(parent_id: &str, unique_id: &str) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(parent_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(unique_id.as_bytes());
    let encoded: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("cec/{encoded}")
}

fn parent_context(
    parent_id: &str,
    label: &str,
    external_rows: &[Value],
    inputs: &[Selectable],
) -> String {
    let input_name = inputs
        .iter()
        .find(|input| input.id == parent_id)
        .map(|input| input.name.trim())
        .filter(|name| !name.is_empty());

    if let Some(name) = input_name {
        // Friendly names are more useful than HDMI port labels. If two
        // physical inputs share a name, retain the port to keep their
        // descendants distinguishable in the selector.
        let duplicate_name = external_rows
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str))
            .filter_map(|id| inputs.iter().find(|input| input.id == id))
            .filter(|input| input.name.trim().eq_ignore_ascii_case(name))
            .count()
            > 1;
        if !duplicate_name {
            return name.to_owned();
        }
        if let Some(port) = parent_id.strip_prefix("HDMI_") {
            if !port.is_empty() && port.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return format!("{name} (HDMI {port})");
            }
        }
    }

    if let Some(port) = parent_id.strip_prefix("HDMI_") {
        if !port.is_empty() && port.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return format!("HDMI {port}");
        }
    }
    if !label.trim().is_empty() {
        label.trim().to_owned()
    } else {
        parent_id.to_owned()
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control)
}

pub(crate) fn append_children(
    external_rows: &[Value],
    eim_rows: &[Value],
    inputs: &mut Vec<Selectable>,
    selections: &mut HashMap<String, CecSelection>,
) {
    for eim_parent in eim_rows {
        // getAllInputStatus uses "id" for the physical input ID and
        // "appId" for the HDMI launch app. Accept "inputId" as well for
        // firmware/fixture variants that expose the external-input shape.
        let Some(parent_input_id) = row_input_id(eim_parent) else {
            continue;
        };
        let Some(external_parent) = external_rows
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(parent_input_id))
        else {
            continue;
        };
        let Some(parent_app_id) = eim_parent
            .get("appId")
            .and_then(Value::as_str)
            .or_else(|| eim_parent.get("id").and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let label = eim_parent
            .get("label")
            .and_then(Value::as_str)
            .or_else(|| external_parent.get("label").and_then(Value::as_str))
            .unwrap_or(parent_input_id);
        let context = parent_context(parent_input_id, label, external_rows, inputs);
        let top_level_names: HashSet<String> = external_rows
            .iter()
            .filter_map(|row| row.get("label").and_then(Value::as_str))
            .map(|name| name.trim().to_lowercase())
            .filter(|name| !name.is_empty())
            .collect();
        let Some(children) = eim_parent.get("subList").and_then(Value::as_array) else {
            continue;
        };

        let mut seen = HashSet::new();
        for child in children {
            let Some(unique_id) = child.get("uniqueId").and_then(scalar) else {
                continue;
            };
            let Some(_depth) = child.get("depth").and_then(Value::as_u64) else {
                continue;
            };
            let name = match child.get("osdName").and_then(Value::as_str) {
                Some(name) => {
                    let name = name.trim();
                    if !valid_name(name) {
                        continue;
                    }
                    name
                }
                None => unique_id.as_str(),
            };
            let child_identity = identity(child);

            // A depth-one EIM entry with a name already present in the TV's
            // top-level input list is represented there; don't duplicate it.
            // This is derived from the current probe, not a hardcoded model name.
            if _depth == 1 && top_level_names.contains(&name.to_lowercase()) {
                continue;
            }

            if let Some(device_identity) = &child_identity {
                if !seen.insert(device_identity.clone()) {
                    continue;
                }
                // A top-level input is the same device only when the TV
                // exposes matching CEC vendor/physical-address identity.
                if external_rows
                    .iter()
                    .any(|row| identity(row).as_ref() == Some(device_identity))
                {
                    continue;
                }
            }

            let id = child_id(parent_input_id, &unique_id);
            if id.len() > 128
                || !couch_sdk::couch_model::commands::valid_input_id(&id)
                || selections.contains_key(&id)
                || inputs.len() >= 128
            {
                continue;
            }
            let display = format!("{context} - {name}");
            if !valid_name(&display) {
                continue;
            }
            inputs.push(Selectable::new(&id, display));
            selections.insert(
                id,
                CecSelection {
                    parent_input_id: parent_input_id.to_owned(),
                    parent_app_id: parent_app_id.to_owned(),
                    unique_id,
                },
            );
        }
    }
}

pub(crate) fn contains_unique_id(
    response: &Value,
    selection: &CecSelection,
    unique_id: &str,
) -> bool {
    response
        .get("devices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|row| row_input_id(row) == Some(selection.parent_input_id.as_str()))
        .and_then(|row| row.get("subList"))
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children
                .iter()
                .any(|child| child.get("uniqueId").and_then(scalar).as_deref() == Some(unique_id))
        })
}

pub(crate) fn last_unique_id(response: &Value, parent_input_id: &str) -> Option<String> {
    response
        .get("devices")?
        .as_array()?
        .iter()
        .find(|row| row_input_id(row) == Some(parent_input_id))?
        .get("lastUniqueId")
        .and_then(scalar)
}

pub(crate) fn launch_payload(selection: &CecSelection) -> Value {
    serde_json::json!({
        "id": selection.parent_app_id,
        "params": {"id": "uniqueId", "value": selection.unique_id}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_top_level_input_and_suppresses_depth_one_name_already_at_top_level() {
        let external = vec![
            json!({"id":"HDMI_2","label":"HDMI 2","appId":"hdmi2"}),
            json!({"id":"CEC_AUDIO","label":"DRAGON"}),
        ];
        let eim = vec![json!({
            "id":"com.webos.app.hdmi2","inputId":"HDMI_2","label":"HDMI 2",
            "lastUniqueId":8,"subList":[
                {"osdName":"DRAGON","uniqueId":3,"vendorId":805705,"physicalAddress":8192,"depth":1},
                {"osdName":"p3i","uniqueId":4,"vendorId":57489,"physicalAddress":8704,"depth":2},
                {"osdName":"p3i duplicate","uniqueId":5,"vendorId":57489,"physicalAddress":8704,"depth":2}
            ]
        })];
        let mut inputs = vec![
            Selectable::new("HDMI_2", "Dragon"),
            Selectable::new("CEC_DRAGON", "DRAGON"),
        ];
        let mut map = HashMap::new();
        append_children(&external, &eim, &mut inputs, &mut map);
        assert!(inputs.iter().any(|i| i.name == "DRAGON"));
        assert!(inputs.iter().any(|i| i.name == "Dragon - p3i"));
        assert_eq!(inputs.len(), 3);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn discovers_children_from_actual_eim_parent_schema() {
        let external = vec![json!({"id":"HDMI_2","label":"HDMI 2","appId":"com.webos.app.hdmi2"})];
        let eim = vec![json!({
            "id":"HDMI_2",
            "appId":"com.webos.app.hdmi2",
            "label":"HDMI 2",
            "lastUniqueId":3,
            "subList":[
                {"osdName":"DRAGON","uniqueId":3,"depth":1},
                {"osdName":"p3i","uniqueId":4,"depth":2}
            ]
        })];
        let mut inputs = vec![Selectable::new("HDMI_2", "HDMI 2")];
        let mut map = HashMap::new();
        append_children(&external, &eim, &mut inputs, &mut map);

        assert!(inputs.iter().any(|input| input.name == "HDMI 2 - p3i"));
        let selection = map
            .values()
            .find(|selection| selection.unique_id == "4")
            .expect("depth-two CEC child selection");
        assert_eq!(selection.parent_input_id, "HDMI_2");
        assert_eq!(selection.parent_app_id, "com.webos.app.hdmi2");
        assert_eq!(selection.unique_id, "4");
    }

    #[test]
    fn duplicate_parent_friendly_names_keep_the_hdmi_port_in_child_labels() {
        let external = vec![
            json!({"id":"HDMI_2","label":"HDMI 2"}),
            json!({"id":"HDMI_3","label":"HDMI 3"}),
        ];
        let eim = vec![
            json!({"id":"hdmi2","inputId":"HDMI_2","label":"HDMI 2","subList":[
                {"osdName":"p3i","uniqueId":4,"depth":2}
            ]}),
            json!({"id":"hdmi3","inputId":"HDMI_3","label":"HDMI 3","subList":[
                {"osdName":"Fire Cube","uniqueId":5,"depth":2}
            ]}),
        ];
        let mut inputs = vec![
            Selectable::new("HDMI_2", "Dragon"),
            Selectable::new("HDMI_3", "Dragon"),
        ];
        let mut map = HashMap::new();
        append_children(&external, &eim, &mut inputs, &mut map);

        assert!(inputs
            .iter()
            .any(|input| input.name == "Dragon (HDMI 2) - p3i"));
        assert!(inputs
            .iter()
            .any(|input| input.name == "Dragon (HDMI 3) - Fire Cube"));
        assert_eq!(map.len(), 2);
        assert_ne!(
            map.get(&child_id("HDMI_2", "4")).unwrap().parent_input_id,
            map.get(&child_id("HDMI_3", "5")).unwrap().parent_input_id
        );
    }

    #[test]
    fn names_alone_never_deduplicate() {
        let external = vec![json!({"id":"HDMI_1","label":"HDMI 1"})];
        let eim = vec![
            json!({"id":"hdmi1","inputId":"HDMI_1","label":"HDMI 1","subList":[
                {"osdName":"Same","uniqueId":1,"depth":2},
                {"osdName":"Same","uniqueId":2,"depth":2}
            ]}),
        ];
        let mut inputs = vec![Selectable::new("HDMI_1", "HDMI 1")];
        let mut map = HashMap::new();
        append_children(&external, &eim, &mut inputs, &mut map);
        assert_eq!(inputs.len(), 3);
    }

    #[test]
    fn launch_and_eim_verification_are_parent_scoped() {
        let selection = CecSelection {
            parent_input_id: "HDMI_2".into(),
            parent_app_id: "com.webos.app.hdmi2".into(),
            unique_id: "4".into(),
        };
        assert_eq!(
            launch_payload(&selection),
            json!({"id":"com.webos.app.hdmi2","params":{"id":"uniqueId","value":"4"}})
        );
        let response = json!({"devices":[{"id":"HDMI_2","appId":"com.webos.app.hdmi2","lastUniqueId":4,"subList":[{"uniqueId":4}]}]});
        assert!(contains_unique_id(&response, &selection, "4"));
        assert_eq!(last_unique_id(&response, "HDMI_2").as_deref(), Some("4"));
    }

    #[test]
    fn malformed_or_incomplete_discovery_rows_are_ignored_without_losing_valid_children() {
        let external = vec![json!({"id":"HDMI_2","label":"HDMI 2"})];
        let eim = vec![
            json!({}),
            json!({"id":"HDMI_9","appId":"hdmi9","subList":[
                {"osdName":"Wrong parent","uniqueId":1,"depth":2}
            ]}),
            json!({"id":"HDMI_2","appId":"","subList":{}}),
            json!({"id":"HDMI_2","appId":"hdmi2","subList":[
                {},
                {"osdName":"Missing depth","uniqueId":1},
                {"osdName":"String depth","uniqueId":2,"depth":"2"},
                {"osdName":"Control\nname","uniqueId":3,"depth":2},
                {"osdName":"Valid child","uniqueId":4,"depth":2}
            ]}),
        ];
        let mut inputs = vec![Selectable::new("HDMI_2", "HDMI 2")];
        let mut selections = HashMap::new();

        append_children(&external, &eim, &mut inputs, &mut selections);

        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[1].name, "HDMI 2 - Valid child");
        assert_eq!(selections.len(), 1);
        assert_eq!(
            selections.values().next().unwrap(),
            &CecSelection {
                parent_input_id: "HDMI_2".into(),
                parent_app_id: "hdmi2".into(),
                unique_id: "4".into(),
            }
        );
    }

    #[test]
    fn eim_verification_is_parent_scoped_and_rejects_incomplete_responses() {
        let selection = CecSelection {
            parent_input_id: "HDMI_2".into(),
            parent_app_id: "com.webos.app.hdmi2".into(),
            unique_id: "4".into(),
        };

        assert!(!contains_unique_id(&json!({}), &selection, "4"));
        assert!(!contains_unique_id(
            &json!({"devices":[{"id":"HDMI_3","subList":[{"uniqueId":4}]}]}),
            &selection,
            "4"
        ));
        assert!(!contains_unique_id(
            &json!({"devices":[{"id":"HDMI_2","subList":{}}]}),
            &selection,
            "4"
        ));
        assert!(!contains_unique_id(
            &json!({"devices":[{"id":"HDMI_2","subList":[{"uniqueId":5}]}]}),
            &selection,
            "4"
        ));
        assert!(contains_unique_id(
            &json!({"devices":[{"id":"HDMI_2","subList":[{"uniqueId":4}]}]}),
            &selection,
            "4"
        ));

        assert_eq!(
            last_unique_id(
                &json!({"devices":[{"id":"HDMI_2","lastUniqueId":4}]}),
                "HDMI_2"
            )
            .as_deref(),
            Some("4")
        );
        assert_eq!(
            last_unique_id(
                &json!({"devices":[{"id":"HDMI_2","lastUniqueId":{}}]}),
                "HDMI_2"
            ),
            None
        );
        assert_eq!(last_unique_id(&json!({"devices":[]}), "HDMI_2"), None);
    }

    #[test]
    fn child_discovery_respects_the_existing_input_limit() {
        let external = vec![json!({"id":"HDMI_2","label":"HDMI 2"})];
        let eim = vec![json!({"id":"HDMI_2","appId":"hdmi2","subList":[
            {"osdName":"Overflow","uniqueId":4,"depth":2}
        ]})];
        let mut inputs: Vec<Selectable> = (0..128)
            .map(|index| Selectable::new(&format!("INPUT_{index}"), "Existing input"))
            .collect();
        let mut selections = HashMap::new();

        append_children(&external, &eim, &mut inputs, &mut selections);

        assert_eq!(inputs.len(), 128);
        assert!(selections.is_empty());
    }
}
