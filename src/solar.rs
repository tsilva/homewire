//! Local sunset computation from NOAA/Meeus equations (90.833° zenith).
use chrono::{DateTime, NaiveDate, Utc};

fn solar_terms(julian: f64) -> (f64, f64) {
    let t = (julian - 2451545.0) / 36525.0;
    let longitude = (280.46646 + t * (36000.76983 + 0.0003032 * t))
        .rem_euclid(360.0)
        .to_radians();
    let anomaly = (357.52911 + t * (35999.05029 - 0.0001537 * t)).to_radians();
    let eccentricity = 0.016708634 - t * (0.000042037 + 0.0000001267 * t);
    let center = anomaly.sin() * (1.914602 - t * (0.004817 + 0.000014 * t))
        + (2.0 * anomaly).sin() * (0.019993 - 0.000101 * t)
        + (3.0 * anomaly).sin() * 0.000289;
    let omega = (125.04 - 1934.136 * t).to_radians();
    let apparent = longitude + (center - 0.00569 - 0.00478 * omega.sin()).to_radians();
    let seconds = 21.448 - t * (46.8150 + t * (0.00059 - 0.001813 * t));
    let obliquity = (23.0 + (26.0 + seconds / 60.0) / 60.0 + 0.00256 * omega.cos()).to_radians();
    let declination = (obliquity.sin() * apparent.sin()).asin();
    let y = (obliquity / 2.0).tan().powi(2);
    let equation = y * (2.0 * longitude).sin() - 2.0 * eccentricity * anomaly.sin()
        + 4.0 * eccentricity * y * anomaly.sin() * (2.0 * longitude).cos()
        - 0.5 * y * y * (4.0 * longitude).sin()
        - 1.25 * eccentricity * eccentricity * (2.0 * anomaly).sin();
    (4.0 * equation.to_degrees(), declination)
}
fn minutes(julian: f64, latitude: f64, longitude: f64) -> Option<f64> {
    let (equation, declination) = solar_terms(julian);
    let latitude = latitude.to_radians();
    let cosine = 90.833_f64.to_radians().cos() / (latitude.cos() * declination.cos())
        - latitude.tan() * declination.tan();
    if !(-1.0..=1.0).contains(&cosine) {
        return None;
    }
    Some(720.0 - 4.0 * (longitude - cosine.acos().to_degrees()) - equation)
}
pub fn sunset(date: NaiveDate, latitude: f64, longitude: f64) -> Option<DateTime<Utc>> {
    if !latitude.is_finite()
        || !longitude.is_finite()
        || latitude.abs() > 89.0
        || longitude.abs() > 180.0
    {
        return None;
    }
    let midnight = date.and_hms_opt(0, 0, 0)?.and_utc();
    let julian = midnight.timestamp() as f64 / 86400.0 + 2440587.5;
    let first = minutes(julian, latitude, longitude)?;
    let refined = minutes(julian + first / 1440.0, latitude, longitude)?;
    midnight.checked_add_signed(chrono::Duration::seconds((refined * 60.0).round() as i64))
}
