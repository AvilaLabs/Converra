//! Bounded capacity models and prescribed-current magnetostatics.

pub mod critical_current;
pub mod filament;
pub mod racetrack;
pub mod tape_frame;

use optcoil_model::{Grade, OperatingPoint};

pub const SCREENING_MODEL_ID: &str = "synthetic-prescribed-field-linear-ic/v1";
pub const LOOP_MODEL_ID: &str = "circular-filament-axis/v1";

/// Lower-bound Ic assumed constant over the stated temperature/angle domain.
/// This is an invented benchmark model, not a fit to measured HTS data.
pub fn tape_capacity_a(grade: &Grade, point: &OperatingPoint) -> Option<f64> {
    let domain = &grade.domain;
    if !(domain.min_temperature_k..=domain.max_temperature_k).contains(&point.temperature_k)
        || !(0.0..=domain.max_field_t).contains(&point.field_t)
        || !(domain.min_angle_deg..=domain.max_angle_deg).contains(&point.field_angle_deg)
    {
        return None;
    }
    let current = grade.ic_at_zero_field_a * (1.0 - grade.field_derating_per_t * point.field_t);
    (current.is_finite() && current > 0.0).then_some(current)
}

/// Bz on the axis of a circular filament, with positive current following the
/// right-hand rule about +z. SI units. Conventional mu0 = 4 pi * 1e-7 H/m.
/// No finite cross section, material response, off-axis field or coil coupling.
/// This reference is NOT used to generate the allocation benchmark's fields.
pub fn circular_loop_axis_bz_t(radius_m: f64, z_m: f64, ampere_turns: f64) -> Option<f64> {
    if !radius_m.is_finite() || radius_m <= 0.0 || !z_m.is_finite() || !ampere_turns.is_finite() {
        return None;
    }
    // The ratio form avoids squaring large dimensional radii directly.
    let distance = radius_m.hypot(z_m);
    let radial_ratio = radius_m / distance;
    let field =
        2.0 * std::f64::consts::PI * 1e-7 * ampere_turns * radial_ratio * radial_ratio / distance;
    field.is_finite().then_some(field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::Case;

    #[test]
    fn never_extrapolates_material_envelope() {
        let case = Case::demo().unwrap();
        let grade = &case.grades[0];
        let mut point = case.modules[0].operating_point.clone();
        assert_eq!(tape_capacity_a(grade, &point), Some(360.0));
        point.field_t = 12.001;
        assert_eq!(tape_capacity_a(grade, &point), None);
        point.field_t = 2.0;
        point.temperature_k = 77.0;
        assert_eq!(tape_capacity_a(grade, &point), None);
        point.temperature_k = 20.0;
        point.field_angle_deg = 90.0;
        assert_eq!(tape_capacity_a(grade, &point), None);
    }

    #[test]
    fn loop_matches_reference_values_and_symmetry() {
        // 1 m radius, 1000 A-turns: mu0 NI / (2R) at center.
        let center = circular_loop_axis_bz_t(1.0, 0.0, 1000.0).unwrap();
        assert!((center - 0.0006283185307179586).abs() < 1e-15);
        let axial = circular_loop_axis_bz_t(1.0, 1.0, 1000.0).unwrap();
        assert!((axial - 0.0002221441469079183).abs() < 1e-15);
        assert_eq!(Some(axial), circular_loop_axis_bz_t(1.0, -1.0, 1000.0));
        assert_eq!(Some(-axial), circular_loop_axis_bz_t(1.0, 1.0, -1000.0));
        assert!(circular_loop_axis_bz_t(0.0, 0.0, 1.0).is_none());
    }
}
