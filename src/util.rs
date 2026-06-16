/// Format a unix timestamp in seconds as an ISO 8601 UTC string.
/// Avoids chrono dependency by hand-rolling a minimal formatter.
/// Only safe for timestamps from 1970 to ~3000 (no leap-second handling).
pub fn format_iso8601(secs: i64) -> String {
    if secs <= 0 {
        return "1970-01-01T00:00:00Z".to_string();
    }
    let days_since_epoch = secs / 86400;
    let secs_of_day = secs % 86400;
    let mut y = 1970i64;
    let mut d = days_since_epoch;
    loop {
        let days_in_year = if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) {
            366
        } else {
            365
        };
        if d < days_in_year {
            break;
        }
        d -= days_in_year;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0usize;
    while m < 12 && d >= month_days[m] {
        d -= month_days[m];
        m += 1;
    }
    let month = m + 1;
    let day = d + 1;
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, month, day, h, min, s
    )
}
