//! Solver-export → [`FieldMap`] conversion. The customer-facing seam: a
//! lab-frame FEA export (rows of `x, y, z, Bx, By, Bz`) becomes the
//! declared Cartesian map the engine consumes. The source file's SHA-256
//! is bound into the case as provenance — the map asserts *the customer's
//! own field solution*, verbatim, never an engine recomputation.
//!
//! Accepted input is deliberately permissive about solver quirks —
//! `#`, `%` or `//` comment lines, a single header line, and comma,
//! semicolon, tab or whitespace separators. Grid levels are the sorted
//! unique coordinate values; every node of the product grid must appear
//! exactly once.

use optcoil_model::coupled::{FieldMap, FieldMapCartesianEntry};

/// Parse a lab-frame solver export into a Cartesian field map.
///
/// `source_sha256` must be the hex SHA-256 of the file bytes as received
/// — the caller hashes so the binding is over the artifact itself, not a
/// re-encoded copy. `reference_ampere_turns_a` is what the declared
/// field was computed at: `1.0` for a per-ampere-turn export, `J·A_pack`
/// for a smeared-current-density solve.
pub fn cartesian_csv_to_field_map(
    text: &str,
    source_sha256: String,
    reference_ampere_turns_a: f64,
) -> Result<FieldMap, String> {
    let mut rows: Vec<[f64; 6]> = Vec::new();
    let mut header_seen = false;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with('%')
            || line.starts_with("//")
        {
            continue;
        }
        let fields: Vec<&str> = line
            .split([',', ';', '\t'])
            .flat_map(|f| f.split_whitespace())
            .filter(|f| !f.is_empty())
            .collect();
        let parsed: Result<Vec<f64>, _> = fields.iter().map(|f| f.parse::<f64>()).collect();
        match parsed {
            Ok(v) if v.len() == 6 => {
                rows.push([v[0], v[1], v[2], v[3], v[4], v[5]]);
            }
            // One leading non-numeric line is a header; anything later
            // is malformed data.
            Err(_) if !header_seen && rows.is_empty() => header_seen = true,
            Ok(v) => {
                return Err(format!(
                    "line {}: expected 6 columns (x y z Bx By Bz), found {}",
                    lineno + 1,
                    v.len()
                ));
            }
            Err(_) => {
                return Err(format!(
                    "line {}: '{line}' — expected numbers or a header row",
                    lineno + 1
                ));
            }
        }
    }
    if rows.is_empty() {
        return Err("no data rows — expected x y z Bx By Bz".to_string());
    }
    let levels = |k: usize| -> Vec<f64> {
        let mut l: Vec<f64> = rows.iter().map(|r| r[k]).collect();
        l.sort_by(f64::total_cmp);
        l.dedup();
        l
    };
    let (x_levels_m, y_levels_m, z_levels_m) = (levels(0), levels(1), levels(2));
    let expect = x_levels_m.len() * y_levels_m.len() * z_levels_m.len();
    if rows.len() != expect {
        return Err(format!(
            "grid is not a complete product: {} nodes parsed but the level sets imply {}×{}×{} = {}",
            rows.len(),
            x_levels_m.len(),
            y_levels_m.len(),
            z_levels_m.len(),
            expect
        ));
    }
    let index_of = |levels: &[f64], v: f64| -> Result<u32, String> {
        levels
            .binary_search_by(|l| l.total_cmp(&v))
            .map(|i| i as u32)
            .map_err(|_| format!("coordinate {v} is not a declared grid node"))
    };
    let mut entries = Vec::with_capacity(rows.len());
    for r in &rows {
        entries.push(FieldMapCartesianEntry {
            x_index: index_of(&x_levels_m, r[0])?,
            y_index: index_of(&y_levels_m, r[1])?,
            z_index: index_of(&z_levels_m, r[2])?,
            bx_t: r[3],
            by_t: r[4],
            bz_t: r[5],
        });
    }
    entries.sort_by_key(|e| (e.x_index, e.y_index, e.z_index));
    // Complete-product check above implies coverage; duplicates would
    // still make neighbouring entries share an index triple.
    for w in entries.windows(2) {
        if (w[0].x_index, w[0].y_index, w[0].z_index) == (w[1].x_index, w[1].y_index, w[1].z_index)
        {
            return Err(format!(
                "duplicate grid node at ({}, {}, {})",
                w[0].x_index, w[0].y_index, w[0].z_index
            ));
        }
    }
    Ok(FieldMap::CartesianBxByBz {
        source_sha256,
        reference_ampere_turns_a,
        x_levels_m,
        y_levels_m,
        z_levels_m,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "abc123";

    fn grid_csv() -> String {
        // 2×2×2 product grid, comma-separated, with a header and a
        // comment — the shape a COMSOL-style export takes.
        let mut s = String::from("# export\nx,y,z,Bx,By,Bz\n");
        for x in [0.0, 0.1] {
            for y in [0.0, 0.1] {
                for z in [0.0, 0.1] {
                    s.push_str(&format!("{x},{y},{z},0.1,0.0,1.2\n"));
                }
            }
        }
        s
    }

    #[test]
    fn parses_a_product_grid() {
        let map = cartesian_csv_to_field_map(&grid_csv(), SHA.into(), 1.0).unwrap();
        let FieldMap::CartesianBxByBz {
            x_levels_m,
            entries,
            source_sha256,
            ..
        } = &map
        else {
            panic!("expected cartesian");
        };
        assert_eq!(x_levels_m, &[0.0, 0.1]);
        assert_eq!(entries.len(), 8);
        assert_eq!(source_sha256, SHA);
        // x outermost: first four entries share x_index 0.
        assert!(entries.iter().take(4).all(|e| e.x_index == 0));
    }

    #[test]
    fn rejects_an_incomplete_grid() {
        // Drop one node — the map must not silently cover a partial grid.
        let mut s = String::new();
        for x in [0.0, 0.1] {
            for y in [0.0, 0.1] {
                for z in [0.0, 0.1] {
                    if z == 0.1 && y == 0.1 && x == 0.1 {
                        continue;
                    }
                    s.push_str(&format!("{x} {y} {z} 0.1 0.0 1.2\n"));
                }
            }
        }
        let err = cartesian_csv_to_field_map(&s, SHA.into(), 1.0).unwrap_err();
        assert!(err.contains("complete product"), "{err}");
    }

    #[test]
    fn rejects_garbage_after_the_header() {
        let err =
            cartesian_csv_to_field_map("x,y,z,Bx,By,Bz\n0,0,0,0,0,1\noops\n", SHA.into(), 1.0)
                .unwrap_err();
        assert!(err.contains("line 3"), "{err}");
    }
}
