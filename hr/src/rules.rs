//! HR rules that are about people at work rather than about dates: the statuses an employee can
//! have and how one becomes another.

/// The statuses an employee can have, and how one becomes another.
pub mod status {
    /// Statuses in which the person works for the organization and counts in its headcount.
    pub const PRESENT: &[&str] = &["active", "probation", "on_leave", "suspended"];
    /// Statuses that end the employment.
    pub const ENDED: &[&str] = &["resigned", "terminated", "retired", "deceased"];

    pub fn is_known(status: &str) -> bool {
        PRESENT.contains(&status) || ENDED.contains(&status)
    }

    pub fn is_ended(status: &str) -> bool {
        ENDED.contains(&status)
    }

    pub fn counts_in_headcount(status: &str) -> bool {
        PRESENT.contains(&status)
    }

    /// Whether `from` may become `to`. An ended employment stays ended: a person who comes back
    /// is hired again, so the history of both stays.
    pub fn can_move(from: &str, to: &str) -> bool {
        if from == to {
            return false;
        }
        match from {
            "probation" => matches!(to, "active" | "suspended" | "resigned" | "terminated"),
            "active" => matches!(to, "on_leave" | "suspended" | "resigned" | "terminated" | "retired" | "deceased"),
            "on_leave" => matches!(to, "active" | "resigned" | "terminated" | "retired" | "deceased"),
            "suspended" => matches!(to, "active" | "terminated" | "resigned"),
            _ => false,
        }
    }

    /// The statuses reachable from `from`, in a stable order, for a form's choices.
    pub fn next_from(from: &str) -> Vec<&'static str> {
        PRESENT.iter().chain(ENDED).copied().filter(|to| can_move(from, to)).collect()
    }
}

/// The youngest age at which a person may be employed. A policy default; an organization with a
/// stricter law checks again in its own layer.
pub const MIN_WORKING_AGE: u32 = 14;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_move_only_along_the_allowed_paths() {
        assert!(status::can_move("probation", "active"));
        assert!(status::can_move("active", "on_leave"));
        assert!(status::can_move("suspended", "terminated"));
        assert!(!status::can_move("active", "active"));
        assert!(!status::can_move("terminated", "active"), "an ended employment stays ended");
        assert!(!status::can_move("probation", "retired"));
        assert!(!status::can_move("nonsense", "active"));
        assert!(status::is_ended("retired") && !status::is_ended("on_leave"));
        assert!(status::counts_in_headcount("suspended") && !status::counts_in_headcount("resigned"));
        assert_eq!(status::next_from("on_leave")[0], "active");
        assert!(status::next_from("deceased").is_empty());
    }
}
