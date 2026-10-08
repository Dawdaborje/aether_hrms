//! The state machine and checks of asset custody, with nothing read from a database.
//!
//! Odoo keeps an equipment's owner as one editable field and a vehicle's odometer as separate editable logs; Frappe
//! has an asset movement document. Here every change is an entry in one ledger that is never edited, the asset's
//! state and holder are only the result of the last entries, and an odometer can never go backwards.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Available,
    Assigned,
    Maintenance,
    Retired,
}

impl State {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "available" => Self::Available,
            "assigned" => Self::Assigned,
            "maintenance" => Self::Maintenance,
            "retired" => Self::Retired,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Assigned => "assigned",
            Self::Maintenance => "maintenance",
            Self::Retired => "retired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Assign,
    Return(Condition),
    StartMaintenance,
    FinishMaintenance,
    Retire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Good,
    Damaged,
    Lost,
}

impl Condition {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "good" => Self::Good,
            "damaged" => Self::Damaged,
            "lost" => Self::Lost,
            _ => return None,
        })
    }
}

/// The state a move leads to, or `None` when the move is not allowed from here.
pub fn next(state: State, mv: Move) -> Option<State> {
    use Move::*;
    use State::*;
    Some(match (state, mv) {
        (Available, Assign) => Assigned,
        (Assigned, Return(Condition::Good)) => Available,
        (Assigned, Return(Condition::Damaged)) => Maintenance,
        (Assigned, Return(Condition::Lost)) => Retired,
        (Available, StartMaintenance) => Maintenance,
        (Maintenance, FinishMaintenance) => Available,
        (Available | Maintenance, Retire) => Retired,
        _ => return None,
    })
}

/// A new odometer reading must not be below the last one.
pub fn odometer_ok(last: Option<i64>, reading: i64) -> Result<(), String> {
    if reading < 0 {
        return Err("an odometer reading cannot be negative".into());
    }
    match last {
        Some(last) if reading < last => Err(format!("the odometer reads {last}: a reading cannot go backwards")),
        _ => Ok(()),
    }
}

/// Distance covered between two readings.
pub fn distance(from: i64, to: i64) -> i64 {
    (to - from).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custody_follows_the_moves_and_nothing_else() {
        assert_eq!(next(State::Available, Move::Assign), Some(State::Assigned));
        assert_eq!(next(State::Assigned, Move::Return(Condition::Good)), Some(State::Available));
        assert_eq!(next(State::Assigned, Move::Return(Condition::Damaged)), Some(State::Maintenance));
        assert_eq!(next(State::Assigned, Move::Return(Condition::Lost)), Some(State::Retired));
        assert_eq!(next(State::Maintenance, Move::FinishMaintenance), Some(State::Available));
        assert_eq!(next(State::Assigned, Move::Assign), None, "no second holder");
        assert_eq!(next(State::Assigned, Move::Retire), None, "returned first");
        assert_eq!(next(State::Retired, Move::Assign), None);
        assert_eq!(next(State::Maintenance, Move::Assign), None);
    }

    #[test]
    fn an_odometer_never_goes_back() {
        assert!(odometer_ok(None, 0).is_ok());
        assert!(odometer_ok(Some(1000), 1000).is_ok());
        assert!(odometer_ok(Some(1000), 999).is_err());
        assert!(odometer_ok(None, -1).is_err());
        assert_eq!(distance(100, 250), 150);
        assert_eq!(distance(250, 100), 0);
    }
}
