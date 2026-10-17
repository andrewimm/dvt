/// Human-readable size in binary units, like `du -h`: "512B", "4.2K", "38M", "1.1G".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1}{}", UNITS[unit])
    } else {
        format!("{v:.0}{}", UNITS[unit])
    }
}

/// Integer with thousands separators: 1234567 -> "1,234,567".
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0B");
        assert_eq!(size(1023), "1023B");
        assert_eq!(size(1024), "1.0K");
        assert_eq!(size(4404019), "4.2M");
        assert_eq!(size(40 * 1024 * 1024 * 1024), "40G");
    }

    #[test]
    fn counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1234567), "1,234,567");
    }
}
