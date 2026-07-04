/// An enum rather than a trait or boxed closure: the set of clocks is
/// closed (the system clock, plus a fixed instant in tests).
pub enum Clock {
    System,
    #[cfg(any(test, feature = "test-util"))]
    Fixed(chrono::DateTime<chrono::Utc>),
}

impl Clock {
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        match self {
            Self::System => chrono::Utc::now(),
            #[cfg(any(test, feature = "test-util"))]
            Self::Fixed(instant) => *instant,
        }
    }

    /// Unix time in seconds; pre-epoch instants clamp to 0.
    pub fn now_unix_secs(&self) -> u64 {
        u64::try_from(self.now().timestamp()).unwrap_or(0)
    }
}
