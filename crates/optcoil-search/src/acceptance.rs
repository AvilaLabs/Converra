//! Recompute candidate quantities and cost without calling search's objective.
//! Capacity uses the shared screening model; this is not independent physics validation.

use optcoil_model::{
    Assessment, Candidate, Case, Check, CostBreakdown, ModelError, ModuleResult, Status,
    allocation_changed,
};
use optcoil_physics::tape_capacity_a;

pub const CHECKER_ID: &str = "allocation-and-cost-checker/v1";

pub fn assess(case: &Case, candidate: &Candidate) -> Result<Assessment, ModelError> {
    case.validate()?;
    case.validate_candidate(candidate)?;
    let mut cost = CostBreakdown::default();
    let mut modules = Vec::new();
    let mut checks = Vec::new();
    for (module, allocation) in case.modules.iter().zip(&candidate.allocations) {
        let grade = case
            .grade(&allocation.grade_id)
            .ok_or_else(|| ModelError::Invalid(format!("unknown grade {}", allocation.grade_id)))?;
        let installed_length_m =
            module.mean_turn_length_m * f64::from(module.turns) * f64::from(allocation.tapes);
        let scrap_length_m = installed_length_m * case.scrap_fraction;
        cost.installed_conductor_usd += installed_length_m * grade.price_usd_per_m;
        cost.scrap_usd += scrap_length_m * grade.price_usd_per_m;
        cost.assembly_usd += case.assembly_cost_per_module_usd;
        let allowed_current_a = tape_capacity_a(grade, &module.operating_point)
            .map(|ic| ic * f64::from(allocation.tapes) * case.utilization_limit);
        let margin = allowed_current_a.map(|allowed| allowed - case.circuit_current_a);
        let (status, detail) = match margin {
            Some(value) if value >= 0.0 => (
                Status::Pass,
                format!("{value:.3} A margin within prescribed synthetic envelope"),
            ),
            Some(value) => (Status::Fail, format!("{value:.3} A current margin")),
            None => (
                Status::Inconclusive,
                "Operating point outside material envelope; no extrapolation".into(),
            ),
        };
        checks.push(Check {
            id: format!("capacity/{}", module.id),
            status,
            detail,
        });
        modules.push(ModuleResult {
            module_id: module.id.clone(),
            installed_length_m,
            purchased_length_m: installed_length_m + scrap_length_m,
            allowed_current_a,
            current_margin_a: margin,
        });
    }
    for (i, interface) in case.interfaces.iter().enumerate() {
        let changed = allocation_changed(&candidate.allocations[i], &candidate.allocations[i + 1]);
        cost.joints_usd += interface.fixed_cost_usd;
        if changed {
            cost.joints_usd += interface.change_cost_usd;
        }
        let allowed = !changed || interface.allow_allocation_change;
        checks.push(Check {
            id: format!(
                "interface/{}->{}",
                interface.left_module_id, interface.right_module_id
            ),
            status: if allowed { Status::Pass } else { Status::Fail },
            detail: if allowed {
                "Permitted module interface"
            } else {
                "Allocation change forbidden at this interface"
            }
            .into(),
        });
    }
    cost.total_usd =
        cost.installed_conductor_usd + cost.scrap_usd + cost.assembly_usd + cost.joints_usd;
    if !cost.total_usd.is_finite() || cost.total_usd <= 0.0 {
        return Err(ModelError::Invalid(
            "computed total cost must remain finite and positive".into(),
        ));
    }
    let screening_status = if checks.iter().any(|c| c.status == Status::Fail) {
        Status::Fail
    } else if checks.iter().any(|c| c.status == Status::Inconclusive) {
        Status::Inconclusive
    } else {
        Status::Pass
    };
    for (id, detail) in [
        (
            "coupled_magnetics",
            "Field is prescribed; allocation-dependent field and finite cross section are not solved",
        ),
        (
            "mechanical",
            "Stress, strain, bend radius, support and pack fit are not evaluated",
        ),
        (
            "thermal",
            "Cooling, AC losses and joint heating are not evaluated",
        ),
        (
            "quench",
            "Quench detection and protection are not evaluated",
        ),
        (
            "manufacturing_validation",
            "Actual winding process, stock lengths and joint geometry need engineering evidence",
        ),
    ] {
        checks.push(Check {
            id: id.into(),
            status: Status::NotEvaluated,
            detail: detail.into(),
        });
    }
    Ok(Assessment {
        candidate: candidate.clone(),
        screening_status,
        engineering_status: if screening_status == Status::Fail {
            Status::Fail
        } else {
            Status::NotEvaluated
        },
        cost,
        modules,
        checks,
    })
}
