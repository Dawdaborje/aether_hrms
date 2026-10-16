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

/// Setup handlers of the owning plugins, offered here so the Configurations app has one place to ask.
macro_rules! forward {
    ($($name:ident => $plugin:literal;)*) => {
        handler! {
            $(
                fn $name(input: Option<Value>) -> Value {
                    plugins::call($plugin, stringify!($name), &input.unwrap_or_else(|| json!({})))
                }
            )*
        }
    };
}

forward! {
    list_departments => "hr";
    list_employees => "hr";
    list_departures => "hr_onboarding";
    bulk_assign_structure_by_department => "hr_payroll";
    create_tax_slab_simple => "hr_payroll";
    list_structures => "hr_payroll";
    bulk_assign_shift_by_department => "hr_attendance";
    mark_attendance_bulk => "hr_attendance";
    list_rotations => "hr_attendance";
    list_rule_packs => "hr_payroll";
    import_rule_pack => "hr_payroll";
    retire_rule_pack => "hr_payroll";
    reload_shipped_rule_packs => "hr_payroll";
    update_shift => "hr_attendance";
    get_shift => "hr_attendance";
    update_location => "hr_attendance";
    get_location => "hr_attendance";
    update_pay_component => "hr_payroll";
    get_pay_component => "hr_payroll";
    update_pay_period => "hr_payroll";
    get_pay_period => "hr_payroll";
    create_rotation_simple => "hr_attendance";
    begin_departure => "hr_onboarding";
    list_departure_reasons => "hr_onboarding";
    turnover_report => "hr";
    waive_notice_shortfall => "hr_onboarding";
    list_letter_templates => "hr_career";
    create_letter_template => "hr_career";
    issue_employee_letter => "hr_career";
    list_employee_letters => "hr_career";
    get_hr_setting => "hr";
    set_hr_setting => "hr";
    get_payroll_setting => "hr_payroll";
    set_payroll_setting => "hr_payroll";
    list_pay_components => "hr_payroll";
    create_pay_component => "hr_payroll";
    list_pay_periods => "hr_payroll";
    create_correction_run => "hr_payroll";
    list_pay_runs => "hr_payroll";
    create_pay_period => "hr_payroll";
    close_pay_period => "hr_payroll";
    reopen_pay_period => "hr_payroll";
    list_tax_slabs => "hr_payroll";
    list_exemption_categories => "hr_payroll";
    create_exemption_category => "hr_payroll";
    list_exemption_declarations => "hr_payroll";
    list_rec_sources => "hr_recruitment";
    create_rec_source => "hr_recruitment";
    list_opening_templates => "hr_recruitment";
    create_opening_template => "hr_recruitment";
    list_offer_terms => "hr_recruitment";
    create_offer_term => "hr_recruitment";
    list_offer_templates => "hr_recruitment";
    update_rec_source => "hr_recruitment";
    get_rec_source => "hr_recruitment";
    update_opening_template => "hr_recruitment";
    get_opening_template => "hr_recruitment";
    update_offer_term => "hr_recruitment";
    get_offer_term => "hr_recruitment";
    update_offer_template => "hr_recruitment";
    get_offer_template => "hr_recruitment";
    create_offer_template_form => "hr_recruitment";
    list_interviewer_pools => "hr_recruitment";
    create_interviewer_pool => "hr_recruitment";
    list_shifts => "hr_attendance";
    create_shift => "hr_attendance";
    list_locations => "hr_attendance";
    create_location => "hr_attendance";
    list_overtime_rules => "hr_attendance";
    list_leave_periods => "hr_leave";
    create_leave_period => "hr_leave";
    close_leave_period => "hr_leave";
    list_leave_approvers => "hr_leave";
    set_leave_approver => "hr_leave";
    list_all_banks => "hr_documents";
    create_bank => "hr_documents";
    get_bank => "hr_documents";
    update_bank => "hr_documents";
    list_all_bank_branches => "hr_documents";
    create_bank_branch => "hr_documents";
    get_bank_branch => "hr_documents";
    update_bank_branch => "hr_documents";
    list_all_work_locations => "hr";
    create_work_location => "hr";
    get_work_location => "hr";
    update_work_location => "hr";
    list_all_cost_centers => "hr";
    create_cost_center => "hr";
    get_cost_center => "hr";
    update_cost_center => "hr";
}
