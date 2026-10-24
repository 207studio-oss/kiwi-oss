use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Finished,
    ReleasePending,
    Incomplete,
    Unconfirmed,
}

impl State {
    pub fn exit_code(self) -> i32 {
        match self {
            State::Finished => 0,
            State::ReleasePending => 3,
            State::Incomplete => 4,
            State::Unconfirmed => 2,
        }
    }
}

#[derive(Serialize)]
pub struct Finding {
    pub state: State,
    pub code: &'static str,
    pub source: String,
}

pub type Result<T> = std::result::Result<T, Finding>;

pub fn finding(state: State, code: &'static str, source: impl Into<String>) -> Finding {
    Finding {
        state,
        code,
        source: source.into(),
    }
}

pub fn unconfirmed(code: &'static str, source: impl Into<String>) -> Finding {
    finding(State::Unconfirmed, code, source)
}
