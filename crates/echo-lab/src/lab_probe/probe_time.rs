use std::time::{SystemTime, UNIX_EPOCH};

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
pub fn now() -> Result<String, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "probe timestamp: clock predates epoch")?
        .as_secs();
    timestamp(seconds)
}

pub fn timestamp(seconds: u64) -> Result<String, String> {
    // Gregorian civil date from days since 1970-01-01, using 400-year eras.
    let days = i64::try_from(seconds / 86_400).map_err(|_| "probe timestamp: out of range")? + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    if year > 9999 {
        return Err("probe timestamp: out of range".into());
    }
    let time = seconds % 86_400;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    ))
}
