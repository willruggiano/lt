use chrono::{DateTime, Utc};

pub enum Clock {
    System,
    #[cfg(any(test, feature = "test-util"))]
    Fixed(DateTime<Utc>),
}

impl Clock {
    pub fn now(&self) -> DateTime<Utc> {
        match self {
            Self::System => Utc::now(),
            #[cfg(any(test, feature = "test-util"))]
            Self::Fixed(instant) => *instant,
        }
    }
}
