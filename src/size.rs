//! CLI and configuration byte sizes. Parse decimal fractions with integer arithmetic.
const MAX_BYTES: u128 = 9_007_199_254_740_991;

pub fn parse_size(input: &str) -> Result<i64, String> {
    let input = input.trim();
    let boundary = input
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(input.len());
    let number = input[..boundary].trim();
    let suffix = input[boundary..].to_ascii_uppercase();
    let power = match suffix.as_str() {
        "" | "B" => 0,
        "K" | "KB" | "KIB" => 1,
        "M" | "MB" | "MIB" => 2,
        "G" | "GB" | "GIB" => 3,
        "T" | "TB" | "TIB" => 4,
        "P" | "PB" | "PIB" => 5,
        _ => {
            return Err(
                "use raw bytes or a binary suffix: K, M, G, T, P (also KiB, MiB, etc.)".into(),
            );
        }
    };
    let (whole, fraction) = match number.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
        Some(_) => return Err("a decimal point must be followed by digits".into()),
        None => (number, ""),
    };
    if (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
        || fraction.len() > 18
    {
        return Err("size must be nonnegative, with at most 18 decimal places".into());
    }
    let out_of_range = || "size must be between 0 and 9007199254740991 bytes".to_owned();
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| out_of_range())?
    };
    if whole > MAX_BYTES {
        return Err(out_of_range());
    }
    let scale = 10_u128.pow(fraction.len() as u32);
    let fraction: u128 = if fraction.is_empty() {
        0
    } else {
        fraction.parse().map_err(|_| out_of_range())?
    };
    if power == 0 && fraction != 0 {
        return Err("raw byte counts must be whole numbers".into());
    }
    let multiplier = 1024_u128.pow(power);
    let whole_bytes = whole * multiplier;
    if whole_bytes > MAX_BYTES {
        return Err(out_of_range());
    }
    let scaled = whole_bytes * scale + fraction * multiplier;
    // Quotas cannot contain fractional bytes; round down without floating-point loss.
    if scaled > MAX_BYTES * scale {
        return Err(out_of_range());
    }
    Ok((scaled / scale) as i64)
}

pub fn format_size(bytes: i64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut unit = 0;
    let mut divisor = 1_u128;
    while unit + 1 < units.len() && bytes as u128 >= divisor * 1024 {
        unit += 1;
        divisor *= 1024;
    }
    let hundredths = (bytes as u128 * 100 + divisor / 2) / divisor;
    let mut value = format!("{}.{:02}", hundredths / 100, hundredths % 100);
    while value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.pop();
    }
    format!("{bytes} ({value} {})", units[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_binary_units_decimals_and_exact_raw_bytes() {
        for (input, expected) in [
            ("0", 0),
            ("53687091200", 53_687_091_200),
            ("9007199254740991", 9_007_199_254_740_991),
            ("1.5G", 1_610_612_736),
            ("20M", 20_971_520),
            ("3K", 3072),
            (" 1.5 gib ", 1_610_612_736),
            (".5T", 549_755_813_888),
            ("1P", 1_125_899_906_842_624),
            ("1.1G", 1_181_116_006),
            ("1kb", 1024),
            ("1.0B", 1),
        ] {
            assert_eq!(parse_size(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn rejects_malformed_negative_and_overflowing_sizes() {
        for input in [
            "",
            "G",
            "-1",
            "-1G",
            "NaN",
            "inf",
            "1e3",
            "1..2G",
            "1.G",
            "1.5",
            "1XB",
            "9007199254740992",
            "8P",
            "9007199254740991P",
            "9007199254740991.123456789012345678P",
            "999999999999999999999999999999999999999999G",
        ] {
            assert!(parse_size(input).is_err(), "{input}");
        }
    }

    #[test]
    fn prints_raw_bytes_and_readable_units() {
        for (input, expected) in [
            (0, "0 (0 B)"),
            (123, "123 (123 B)"),
            (3072, "3072 (3 KiB)"),
            (1_610_612_736, "1610612736 (1.5 GiB)"),
            (53_687_091_200, "53687091200 (50 GiB)"),
        ] {
            assert_eq!(format_size(input), expected);
        }
    }
}
