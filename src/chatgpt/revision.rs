//! Exact decimal ordering for native revisions; no binary floating-point ties.
use anyhow::{Context, Result, ensure};
use serde_json::{Number, Value};
use std::cmp::Ordering;
#[derive(Clone, Debug)]
pub(super) struct Revision {
    pub number: Number,
    digits: String,
    magnitude: i64,
    negative: bool,
}
impl Revision {
    pub fn new(value: &Value) -> Result<Self> {
        let number = value
            .as_number()
            .context("ChatGPT update_time must be numeric")?
            .clone();
        let text = number.to_string();
        let negative = text.starts_with('-');
        let text = text.trim_start_matches('-');
        let (mantissa, exponent) =
            text.split_once(['e', 'E'])
                .map_or(Ok((text, 0i64)), |(m, e)| {
                    Ok::<_, anyhow::Error>((
                        m,
                        e.parse().context("revision exponent out of range")?,
                    ))
                })?;
        let fraction = mantissa.split_once('.').map_or(0, |(_, f)| f.len());
        let digits = mantissa
            .replace('.', "")
            .trim_start_matches('0')
            .to_string();
        let magnitude = exponent
            .checked_sub(fraction as i64)
            .and_then(|e| e.checked_add(digits.len() as i64))
            .context("revision magnitude out of range")?;
        ensure!(
            number.as_f64().is_some_and(f64::is_finite),
            "invalid ChatGPT update_time"
        );
        Ok(Self {
            number,
            digits,
            magnitude,
            negative,
        })
    }
}
impl PartialEq for Revision {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Revision {}
impl PartialOrd for Revision {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Revision {
    fn cmp(&self, other: &Self) -> Ordering {
        let sign = |v: &Self| {
            if v.digits.is_empty() {
                0
            } else if v.negative {
                -1
            } else {
                1
            }
        };
        let order = sign(self).cmp(&sign(other));
        if order != Ordering::Equal || sign(self) == 0 {
            return order;
        }
        let order = self.magnitude.cmp(&other.magnitude).then_with(|| {
            let width = self.digits.len().max(other.digits.len());
            self.digits
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(width)
                .cmp(
                    other
                        .digits
                        .bytes()
                        .chain(std::iter::repeat(b'0'))
                        .take(width),
                )
        });
        if self.negative {
            order.reverse()
        } else {
            order
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn revision(text: &str) -> Revision {
        Revision::new(&serde_json::from_str::<Value>(text).unwrap()).unwrap()
    }
    #[test]
    fn decimal_order_handles_signs_zero_and_equivalent_exponent_spellings() {
        assert_eq!(revision("1e3"), revision("1000.00"));
        assert_eq!(revision("-0.0"), revision("0e2"));
        let values = [
            "-100", "-1.001", "-1", "-0.001", "0", "0.001", "1", "1.001", "100",
        ];
        for pair in values.windows(2) {
            assert!(revision(pair[0]) < revision(pair[1]), "{pair:?}");
        }
    }
}
