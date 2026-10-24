type Result<T> = std::result::Result<T, String>;

pub fn timestamp(s: &str) -> Result<u64> {
    fn n(s: &str) -> Result<i64> {
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid ISO timestamp digits".into());
        }
        s.parse().map_err(|_| "invalid ISO timestamp".into())
    }
    if !s.is_ascii() || s.len() < 20 {
        return Err("expected ISO timestamp with timezone".into());
    }
    if &s[4..5] != "-"
        || &s[7..8] != "-"
        || &s[10..11] != "T"
        || &s[13..14] != ":"
        || &s[16..17] != ":"
    {
        return Err("invalid ISO timestamp".into());
    }
    let (y, m, d, h, min, sec) = (
        n(&s[..4])?,
        n(&s[5..7])?,
        n(&s[8..10])?,
        n(&s[11..13])?,
        n(&s[14..16])?,
        n(&s[17..19])?,
    );
    let leap = |y: i64| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    if !(1970..=2200).contains(&y)
        || !(1..=12).contains(&m)
        || !(0..24).contains(&h)
        || !(0..60).contains(&min)
        || !(0..60).contains(&sec)
    {
        return Err("invalid ISO timestamp range".into());
    }
    let months = [
        31,
        if leap(y) { 29 } else { 28 },
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
    if d < 1 || d > months[(m - 1) as usize] {
        return Err("invalid ISO day".into());
    }
    let days: i64 = (1970..y)
        .map(|v| if leap(v) { 366 } else { 365 })
        .sum::<i64>()
        + months[..(m - 1) as usize].iter().sum::<i64>()
        + d
        - 1;
    let zone = match s[19..].strip_prefix('.') {
        Some(fraction) => {
            let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
            if !(1..=9).contains(&digits) {
                return Err("invalid fractional seconds".into());
            }
            &fraction[digits..]
        }
        None => &s[19..],
    };
    let offset = if zone == "Z" {
        0
    } else if zone.len() == 6
        && (zone.starts_with('+') || zone.starts_with('-'))
        && &zone[3..4] == ":"
    {
        let (oh, om) = (n(&zone[1..3])?, n(&zone[4..6])?);
        if oh > 23 || om > 59 {
            return Err("invalid timezone offset".into());
        }
        (oh * 3600 + om * 60) * if zone.starts_with('+') { 1 } else { -1 }
    } else {
        return Err("timezone required".into());
    };
    u64::try_from(days * 86400 + h * 3600 + min * 60 + sec - offset)
        .map_err(|_| "timestamp before epoch".into())
}
