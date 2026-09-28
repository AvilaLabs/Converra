//! Text-field parsers extracted from `author.rs` — the guided case
//! builder's comma/pair/group syntaxes. Pure `&str -> Result` seams so
//! error handling is unit-testable headless; every message names the
//! field and the rejected token.

use optcoil_model::coupled_search::{PieceOffering, PriceSource, RelativeTurnIndex};

/// Parse a comma-separated u32 list ("120, 200, 240").
pub(super) fn parse_u32_list(text: &str, name: &str) -> Result<Vec<u32>, String> {
    text.split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse::<u32>()
                .map_err(|_| format!("{name}: '{t}' is not a positive integer"))
        })
        .collect()
}

/// Parse a comma-separated f64 list ("0.09, 0.12, 0.15").
pub(super) fn parse_f64_list(text: &str, name: &str) -> Result<Vec<f64>, String> {
    text.split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse::<f64>()
                .map_err(|_| format!("{name}: '{t}' is not a number"))
        })
        .collect()
}

/// Parse a comma-separated table of `T:value` pairs ("300:0.2, 350:0.5")
/// — the declared property tables the quench screens carry.
pub(super) fn parse_f64_pairs(text: &str, name: &str) -> Result<Vec<[f64; 2]>, String> {
    text.split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| {
            let (a, b) = t
                .split_once(':')
                .ok_or_else(|| format!("{name}: '{t}' — expected 'T:value'"))?;
            Ok([
                a.trim()
                    .parse::<f64>()
                    .map_err(|_| format!("{name}: '{a}' is not a number"))?,
                b.trim()
                    .parse::<f64>()
                    .map_err(|_| format!("{name}: '{b}' is not a number"))?,
            ])
        })
        .collect()
}

/// Parse the relative-turn-index shorthand:
/// `start:1,2,3,5  frac:0.25,0.5,0.75  end:19,9,4,1,0` — any subset of the
/// three groups, in any order.
pub(super) fn parse_turn_indices(text: &str) -> Result<Vec<RelativeTurnIndex>, String> {
    let mut out = Vec::new();
    for group in text.split_whitespace() {
        let Some((kind, list)) = group.split_once(':') else {
            return Err(format!(
                "turn indices: '{group}' — expected kind:list (start:/frac:/end:)"
            ));
        };
        for item in list.split(',').filter(|t| !t.trim().is_empty()) {
            let item = item.trim();
            out.push(match kind {
                "start" => RelativeTurnIndex::FromStart {
                    offset: item
                        .parse::<u32>()
                        .map_err(|_| format!("turn indices: '{item}' is not an integer"))?,
                },
                "end" => RelativeTurnIndex::FromEnd {
                    offset: item
                        .parse::<u32>()
                        .map_err(|_| format!("turn indices: '{item}' is not an integer"))?,
                },
                "frac" => RelativeTurnIndex::Fraction {
                    value: item
                        .parse::<f64>()
                        .map_err(|_| format!("turn indices: '{item}' is not a fraction"))?,
                },
                other => {
                    return Err(format!(
                        "turn indices: unknown group '{other}' (start:/frac:/end:)"
                    ));
                }
            });
        }
    }
    if out.is_empty() {
        return Err("turn indices: at least one index is required".into());
    }
    Ok(out)
}

/// Parse `length_m : $/m` comma pairs into a piece-offering catalogue
/// (`None` when empty). Used for both the case-level catalogue and each
/// tape spec's own offerings.
pub(super) fn parse_offerings(text: &str) -> Result<Option<Vec<PieceOffering>>, String> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (len, price) = part
            .split_once(':')
            .ok_or_else(|| format!("piece offering '{part}' — expected length_m : $/m"))?;
        out.push(PieceOffering {
            length_m: len
                .trim()
                .parse()
                .map_err(|_| format!("piece offering '{part}' — bad length"))?,
            price_usd_per_m: price
                .trim()
                .parse()
                .map_err(|_| format!("piece offering '{part}' — bad price"))?,
        });
    }
    Ok((!out.is_empty()).then_some(out))
}

/// The price-source combo index → schema class (0 = undeclared).
pub(super) fn price_source(i: usize) -> Option<PriceSource> {
    match i {
        1 => Some(PriceSource::Synthetic),
        2 => Some(PriceSource::Estimated),
        3 => Some(PriceSource::Published),
        4 => Some(PriceSource::Quoted),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u32_list_parses_trims_and_skips_blanks() {
        assert_eq!(
            parse_u32_list("120, 200,, 240", "x").unwrap(),
            [120, 200, 240]
        );
        assert_eq!(parse_u32_list("", "x").unwrap(), Vec::<u32>::new());
        let e = parse_u32_list("4, nope", "turns").unwrap_err();
        assert!(e.contains("turns") && e.contains("nope"));
        assert!(parse_u32_list("-1", "x").is_err());
        assert!(parse_u32_list("1.5", "x").is_err());
    }

    #[test]
    fn f64_list_parses_and_rejects() {
        assert_eq!(parse_f64_list("0.09, 0.12", "x").unwrap(), [0.09, 0.12]);
        let e = parse_f64_list("0.1, nope", "radii").unwrap_err();
        assert!(e.contains("radii") && e.contains("nope"));
    }

    #[test]
    fn f64_pairs_require_a_colon() {
        assert_eq!(
            parse_f64_pairs("300:0.2, 350:0.5", "x").unwrap(),
            [[300.0, 0.2], [350.0, 0.5]]
        );
        assert!(parse_f64_pairs("300", "x").is_err());
        assert!(parse_f64_pairs("a:0.2", "x").is_err());
        assert!(parse_f64_pairs("300:b", "x").is_err());
    }

    #[test]
    fn turn_indices_parse_all_three_groups() {
        let idx = parse_turn_indices("start:1,2 frac:0.5 end:0").unwrap();
        assert_eq!(idx.len(), 4);
        assert!(matches!(idx[0], RelativeTurnIndex::FromStart { offset: 1 }));
        assert!(matches!(idx[2], RelativeTurnIndex::Fraction { .. }));
        assert!(matches!(idx[3], RelativeTurnIndex::FromEnd { offset: 0 }));
        assert!(parse_turn_indices("bogus:1").is_err());
        assert!(parse_turn_indices("").is_err());
        assert!(parse_turn_indices("start:").is_err());
        assert!(parse_turn_indices("start:x").is_err());
        assert!(parse_turn_indices("nocolon").is_err());
    }

    #[test]
    fn offerings_parse_pairs_and_empty_is_none() {
        assert!(parse_offerings("").unwrap().is_none());
        assert!(parse_offerings(" , ,").unwrap().is_none());
        let o = parse_offerings("120:30, 240:28").unwrap().unwrap();
        assert_eq!(o[0].length_m, 120.0);
        assert_eq!(o[1].price_usd_per_m, 28.0);
        assert!(parse_offerings("120").is_err());
        assert!(parse_offerings("120:x").is_err());
    }

    #[test]
    fn price_source_maps_combo_indices() {
        assert!(price_source(0).is_none());
        assert!(price_source(99).is_none());
        assert!(matches!(price_source(1), Some(PriceSource::Synthetic)));
        assert!(matches!(price_source(4), Some(PriceSource::Quoted)));
    }
}
