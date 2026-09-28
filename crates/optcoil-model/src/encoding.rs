//! Canonical-profile dual-acceptance for case documents.
//!
//! Canonical JSON profiles carry exact decimals as strings rather than
//! binary floats, so a case authored under such a profile looks like
//! `"b_target_t": "0.9"`. Parsing is two-pass: the native form is
//! tried first and, only if it fails, every string that parses as a finite
//! `f64` is normalized to a JSON number and the document is retried. Input
//! that parsed before is never reinterpreted; normalization can only make a
//! previously invalid document parse, never change a valid one's meaning.
//! The record's `case_sha256` still hashes the raw authored bytes, so a
//! canonical-profile case's staged-artifact identity and the run record's
//! case identity are the same digest.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ModelError;

pub(crate) fn parse_case<T: DeserializeOwned>(json: &str) -> Result<T, ModelError> {
    match serde_json::from_str(json) {
        Ok(case) => Ok(case),
        Err(_) => {
            let mut value: Value = serde_json::from_str(json)?;
            normalize_decimal_strings(&mut value);
            Ok(serde_json::from_value(value)?)
        }
    }
}

fn normalize_decimal_strings(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Ok(number) = text.parse::<f64>()
                && number.is_finite()
            {
                // Integral decimals ("3", "3.0") must become JSON integers:
                // a float `3.0` cannot satisfy a `u32` field, while an
                // integer `3` deserializes into either `u32` or `f64`.
                *value = if number.fract() == 0.0
                    && number >= i64::MIN as f64
                    && number <= i64::MAX as f64
                {
                    Value::from(number as i64)
                } else {
                    Value::from(number)
                };
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_decimal_strings(item);
            }
        }
        Value::Object(members) => {
            for member in members.values_mut() {
                normalize_decimal_strings(member);
            }
        }
        _ => {}
    }
}
