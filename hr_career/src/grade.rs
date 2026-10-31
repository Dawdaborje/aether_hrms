//! The grade ladder lives in `hr` (HR Configuration): a grade has a rank and, optionally, a band the wage must stay
//! inside. Terms keep the grade's code. `career_grade` is the ladder this plugin used to keep; it is only read, once,
//! to carry its grades over.

use aether_sdk::prelude::*;

use crate::common::{text, Record};

/// The grade with this code.
pub fn by_code(code: &str) -> Result<Option<Record>> {
    plugins::call("hr", "grade_by_code", &json!({ "code": code }))
}

/// Carry the grades of the old `career_grade` ladder over to `hr` (idempotent: a code that exists there is kept).
fn run_migrate_grades() -> Result<Value> {
    let mut moved = 0;
    for old in db::find::<Record>("career_grade").order_by("rank").limit(500).all()? {
        let Some(code) = text(&old, "code") else { continue };
        if by_code(code)?.is_some() {
            continue;
        }
        let _: Record = plugins::call("hr", "create_grade", &old)?;
        moved += 1;
    }
    Ok(json!({ "grades": moved }))
}

handler! {
    fn migrate_career_grades(_: Empty) -> Value {
        run_migrate_grades()
    }
}
