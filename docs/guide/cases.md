# Geometry and operating requirements

A case declares the engineering question: field requirement, clear aperture or good-field region, operating current and temperature, allowed winding choices, material bindings, numerical settings and costs. Set these before comparing designs.

## Choose a supported geometry

| Geometry | Field evaluation |
| --- | --- |
| Planar racetrack | Built-in finite-cross-section evaluator |
| Circular path | Built-in evaluator |
| Planar line-and-arc path | Built-in path evaluator |
| Non-planar helix/path | Requires a declared field map |

The guided workbench builder authors supported racetrack and helix cases. Loaded cases expose structured revision controls and an advanced JSON editor for other declarations. A general CAD solid is not automatically a supported winding case.

For a non-planar path, bind the field map and its declared geometry/current convention. Coverage, interpolation and conductor orientation remain part of the screened problem. The [helix benchmark](https://github.com/AvilaLabs/Converra/blob/main/docs/OC031.md) documents this workflow.

## Define what can change

The candidate grid bounds the search. Turn counts, tape counts, parallel strands and permitted geometry parameters are choices within that grid. Grading additionally declares the conductor specifications and allowed winding regions or interfaces.

Use consistent requirements when ranking alternatives. A larger aperture, lower field, different operating temperature or altered sampling gate changes the design problem. The study comparison displays these changes; scenario ranking rejects incompatible contracts.

## Check applicability first

Use the workbench's applicability panel or `optcoil preflight case.json` before dispatch. Resolve missing datasets and invalid declarations. Preflight checks input applicability and estimated workload; it cannot prove that the solved peak conductor fields stay inside the material domain.

Keep sampling and search limits explicit. Refined selection checks can reveal a limiting point that the coarse candidate screen missed.

Detailed schema and engine structure: [architecture](https://github.com/AvilaLabs/Converra/blob/main/docs/ARCHITECTURE.md).
