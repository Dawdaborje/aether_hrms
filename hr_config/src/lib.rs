//! HRMS Configurations app: thin UI over setup functions in sibling plugins.

use aether_sdk::prelude::*;
use aether_sdk::records::Record;

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct Change {
    id: String,
    #[serde(flatten)]
    fields: Record,
}

handler! {
    /// Active + inactive leave types for the Configurations list.
    fn list_all_leave_types(_: Empty) -> Vec<Record> {
        plugins::call("hr_leave", "list_all_leave_types", &json!({}))
    }

    fn get_leave_type(input: Id) -> Option<Record> {
        plugins::call("hr_leave", "get_leave_type", &json!({ "id": input.id }))
    }

    fn create_leave_type(input: Record) -> Record {
        plugins::call("hr_leave", "create_leave_type", &input)
    }

    fn update_leave_type(input: Change) -> Record {
        let mut payload = input.fields;
        payload["id"] = json!(input.id);
        plugins::call("hr_leave", "update_leave_type", &payload)
    }
}
