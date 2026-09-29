use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use clap::{Args, Parser, Subcommand};
use optcoil_adapters::kernel_crosscheck::{
    BluemiraCrosscheck, KERNEL_PROBE_DECK_SCHEMA, KernelProbeRequest, ProbeGeometry,
};
use optcoil_adapters::{PLANNED_ADAPTERS, SolverAdapter};
use optcoil_model::{
    Case, Status,
    attestation::{
        DatasetAttestation, DatasetRegistry, RegistryEntry, RegistryKey, SigningKeyFile,
        attestation_sha256, sha256_hex,
    },
    material::{MaterialBenchmark, MaterialBundle, MaterialDataset, MaterialQuery},
};
use optcoil_search::{
    RunRecord, SearchOptions, acceptance,
    coupled::{CoupledOptions, CoupledRunRecord, run_coupled_case, run_oc004},
    coupled_refine::{CoupledRefineOptions, CoupledRefineRunRecord, run_oc008},
    coupled_search::{
        CoupledSearchOptions, CoupledSearchRunRecord, SearchProgress,
        run_coupled_search_case_with_dataset_progress, run_oc007,
    },
    field::{FieldOptions, FieldRunRecord, run_field_case, run_oc002},
    kernel_crosscheck::run_kernel_crosscheck,
    material::{
        MaterialRunRecord, MaterialSuiteRecord, query_embedded_material_by_id, query_material,
        run_material_benchmark, run_material_suite,
    },
    run,
};
use rand_core::RngCore;

#[derive(Parser)]
#[command(
    name = "optcoil",
    version,
    about = "OptCoil — coil allocation and cost optimization workbench"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the embedded synthetic reference benchmark (no file required).
    Demo(RunArgs),
    /// Run OC-002 racetrack fields against the embedded independent reference.
    FieldBenchmark(FieldArgs),
    /// Validate measured conductor interpolation, baseline and reserved challenge.
    MaterialBenchmark(ReportArgs),
    /// Run any material benchmark JSON (single-axis or OC-005's simultaneous
    /// multi-axis fold) against the embedded dataset by default. Exit code
    /// is nonzero unless interpolation_validation_status is PASS.
    MaterialValidate {
        benchmark: PathBuf,
        /// Selects an embedded dataset by id, overriding the benchmark's own
        /// declared dataset_id; rejected explicitly if it does not match.
        /// Ignored (and rejected) alongside --metadata/--csv.
        #[arg(long, conflicts_with_all = ["metadata", "csv"])]
        dataset: Option<String>,
        /// Omit both paths to use an embedded dataset (the benchmark's own
        /// declared dataset_id by default; override with --dataset).
        #[arg(long, requires = "csv")]
        metadata: Option<PathBuf>,
        #[arg(long, requires = "metadata")]
        csv: Option<PathBuf>,
        #[command(flatten)]
        report: ReportArgs,
    },
    /// Query measured Ic per width using applied field and angle from tape normal.
    MaterialQuery(MaterialQueryArgs),
    /// Validate an attributed material metadata/CSV pair and inspect its coverage.
    MaterialInspect {
        metadata: PathBuf,
        csv: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Package a validated material metadata/CSV pair as a single-file
    /// dataset bundle (optcoil-material-dataset/v1) for `coupled-search
    /// --dataset-bundle` or the workbench's "Load dataset" flow. The
    /// bundle is validated before it is written: metadata.csv_sha256 must
    /// match the CSV bytes.
    DatasetBundle {
        metadata: PathBuf,
        csv: PathBuf,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Dataset custodianship: build, sign and verify attested dataset
    /// bundles (optcoil-material-dataset/v2) and dataset registries.
    Dataset {
        #[command(subcommand)]
        command: DatasetCommand,
    },
    /// Evaluate a magnetic case; optional matching reference enables validation.
    /// Exit code is nonzero unless numerical validation passes.
    Field {
        case: PathBuf,
        #[arg(long)]
        reference: Option<PathBuf>,
        #[command(flatten)]
        args: FieldArgs,
    },
    /// Run the embedded OC-004 coupled conductor-geometry screening benchmark.
    CoupledBenchmark(CoupledArgs),
    /// Evaluate a coupled conductor-geometry case; optional matching reference
    /// enables validation. Exit code is nonzero unless numerical validation,
    /// coverage and every candidate all PASS.
    Coupled {
        case: PathBuf,
        #[arg(long)]
        reference: Option<PathBuf>,
        #[command(flatten)]
        args: CoupledArgs,
    },
    /// Run the embedded OC-007 coupled cost search benchmark.
    CoupledSearchBenchmark(CoupledSearchArgs),
    /// Search permitted racetrack pack geometry (turns along normal x tapes
    /// along width) for a lower-modeled-cost candidate under a fixed
    /// bore-field requirement, screened through the OC-004 coupled runner.
    /// Modeled, synthetic prices, screening model — never a production
    /// claim. Exit code is nonzero unless search_status PASS and the
    /// acceptance agreement is also PASS.
    CoupledSearch {
        case: PathBuf,
        #[command(flatten)]
        args: CoupledSearchArgs,
    },
    /// Check applicability and estimate declared work before running a study.
    /// Readiness does not establish field coverage or engineering acceptance.
    Preflight {
        case: PathBuf,
        #[command(flatten)]
        args: CoupledSearchArgs,
    },
    /// Export an exact-input review package with datasets and hash manifest.
    ReviewPackage {
        record: PathBuf,
        case: PathBuf,
        #[arg(long, short)]
        output: PathBuf,
        /// Write a single uncompressed tar archive instead of a new directory.
        #[arg(long)]
        archive: bool,
        #[arg(long)]
        dataset_bundle: Vec<PathBuf>,
    },
    /// Verify every review-package artifact and its case/dataset/ledger bindings.
    VerifyPackage { directory: PathBuf },
    /// Run a declared sensitivity sweep (optcoil-sensitivity/v1, /v2 or
    /// /v3) over a coupled-search case: a full-factorial grid of ic_scale /
    /// temperature_k / price_usd_per_m perturbations (v2 adds
    /// requirement_b_target_t — a certified-ceiling ladder; v3 adds
    /// utilization_limit — the capacity-margin frontier), each point an
    /// ordinary search of the mutated case. Prints one line per point;
    /// --output saves the complete sweep record.
    Sensitivity {
        case: PathBuf,
        spec: PathBuf,
        #[command(flatten)]
        args: CoupledSearchArgs,
    },
    /// Run a vendor comparison: the same coupled-search case run once per
    /// declared entry, ranked into a comparison record. v1 specs list
    /// datasets (materials question: "which measured tape performs
    /// best"); v2 specs list products with real tape width, declared
    /// price + provenance, optional piece catalogues and policy
    /// overrides (procurement question: "which product gives the
    /// cheapest accepted design", ranked by total cost). Every row's
    /// acceptance recomputation stands alone. Prints the ranking;
    /// --output saves the complete record.
    Bakeoff {
        case: PathBuf,
        spec: PathBuf,
        #[command(flatten)]
        args: BakeoffArgs,
    },
    /// Build a grading comparison report (optcoil-grading-report/v1) from
    /// a completed coupled-search run record: per spec id, the cheapest
    /// PASS candidate using that spec on every region, next to the
    /// search's own optimum — "premium tape only where the field demands
    /// it" as a modeled dollar figure. Reads the record; never recomputes.
    GradeReport {
        record: PathBuf,
        /// Save the report JSON; existing files are protected.
        #[arg(long, short)]
        output: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Build the bill of materials (optcoil-bom/v1) a completed coupled
    /// search's optimum implies: conductor metres per spec and contiguous
    /// turn range, installed vs purchased length under the declared scrap
    /// fraction, joint and pancake counts, and the ledger cost split per
    /// spec. A modeled procurement document, not a purchase order.
    Bom {
        record: PathBuf,
        /// Save the BOM JSON; existing files are protected.
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Also write the procurement-facing markdown — the piece
        /// schedule, joints and totals a buyer can attach to an RFQ.
        #[arg(long)]
        rfq: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Run the embedded OC-008 bracketing refinement of OC-007's cost
    /// optimum (schema v2, `refinement` block required).
    CoupledRefineBenchmark(CoupledSearchArgs),
    /// Refine a schema-v2 coupled search case: per-pancake-count bracketing
    /// bisection of the smallest passing turn count, then the cheapest
    /// across pancake counts, screened through the OC-004 coupled runner
    /// and OC-007's own acceptance module. Modeled, synthetic prices,
    /// screening model — never a production claim. Exit code is nonzero
    /// unless search_status PASS and the acceptance agreement is also PASS.
    CoupledRefine {
        case: PathBuf,
        #[command(flatten)]
        args: CoupledSearchArgs,
    },
    /// Render a coupled-search run record as a self-contained HTML
    /// evidence report (verdicts, candidate ledger, acceptance, run
    /// identity, limitations). Readable counterpart to the JSON record.
    Report {
        record: PathBuf,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Reprice a coupled-search run record at a different conductor price.
    /// Exact arithmetic on the record's per-candidate cost ledgers — no
    /// physics reruns, and the emitted note says so: every verdict is the
    /// source record's, bound by its sha256.
    Reprice {
        record: PathBuf,
        /// New conductor price in $/m.
        #[arg(long)]
        price_usd_per_m: f64,
        /// Where this price comes from (published band, supplier quote…).
        /// Required — an unsourced dollar figure is not evidence.
        #[arg(long)]
        price_provenance: String,
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Verify a coupled-search run record's hash bindings and ledger
    /// arithmetic — artifact integrity, not physics. Checks that the
    /// record's declared SHA-256 bindings recompute against supplied
    /// artifacts and that every candidate ledger is internally consistent.
    /// `tools/verify_record.py` is the dependency-free reference.
    Verify {
        record: PathBuf,
        /// Case file to bind against the record's embedded case and its
        /// `case_sha256` (a hash of the case file's raw bytes).
        case: Option<PathBuf>,
        /// Evidence manifest binding the case and run-record artifacts.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// optcoil-material-dataset/v1 bundle to bind against the record's
        /// declared dataset identities. Repeatable — graded records declare
        /// one identity per tape spec (run schema v13+).
        #[arg(long)]
        dataset: Vec<PathBuf>,
    },
    /// Generate a declared Cartesian field map for a `path3d`
    /// coupled-search case with the filament Biot-Savart model, and
    /// write the case back out with `field_map` declared plus the CSV
    /// source sidecar the sha256 binds. This is a filament-model map —
    /// label it `optcoil-filament/v1` in the case provenance; it is not
    /// a substitute for a solver export on a production winding.
    FieldMap {
        case: PathBuf,
        /// Write the updated case JSON here (never overwrites).
        #[arg(long)]
        emit_case: PathBuf,
        /// Write the provenance CSV whose sha256 the declaration binds.
        #[arg(long)]
        source: PathBuf,
        /// Grid pitch in metres (levels auto-cover the swept pack hull
        /// plus margin).
        #[arg(long, default_value_t = 0.005)]
        spacing_m: f64,
        /// Hull margin in metres.
        #[arg(long, default_value_t = 0.01)]
        margin_m: f64,
        /// Arc-length samples per filament. Convergence needs ~1e4+ on
        /// long helixes.
        #[arg(long, default_value_t = 24_000)]
        samples: usize,
        /// True total ampere-turns the map is evaluated at. Any
        /// positive value is a valid scale — the engine scales entries
        /// linearly by candidate NI — but 1.0 reads as a
        /// per-ampere-turn export.
        #[arg(long, default_value_t = 1.0)]
        reference_ni: f64,
    },
    /// Optimize a versioned JSON case with bounded exact enumeration.
    Run {
        case: PathBuf,
        #[command(flatten)]
        args: RunArgs,
    },
    /// Validate a case and assess its baseline without optimization.
    Inspect {
        case: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// OC-011 kernel cross-check: run a frozen probe deck through our
    /// evaluator and an independent magnetostatics (Bluemira), and print
    /// the verdict. `--deck` runs a prepared deck; `--record` builds one
    /// from a coupled-search run record's best candidate (its geometry,
    /// solved NI, and the region lattice + bore probe).
    Crosscheck {
        #[arg(long, conflicts_with = "record")]
        deck: Option<PathBuf>,
        #[arg(long)]
        record: Option<PathBuf>,
        /// Evaluation model for a deck built from --record:
        /// thin_filament (Phase A) or finite_cross_section (Phase B —
        /// the model the search verdicts run on).
        #[arg(long, default_value = "thin_filament", value_parser = ["thin_filament", "finite_cross_section"])]
        evaluation_model: String,
        /// Quadrature order for our own evaluator on the deck.
        #[arg(long, default_value_t = 14, value_parser = clap::value_parser!(u32).range(2..=24))]
        order: u32,
        /// Python interpreter for the Bluemira runner (e.g. a venv's
        /// python); defaults to `python3` on PATH.
        #[arg(long)]
        python: Option<String>,
        /// Emit the complete cross-check record as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// List planned external integrations and their actual availability.
    Adapters,
}

#[derive(Args)]
struct RunArgs {
    #[arg(long, default_value_t = 100_000, value_parser = clap::value_parser!(u64).range(1..))]
    max_evaluations: u64,
    /// Write a complete JSON record. Existing files are never overwritten.
    #[arg(long, short)]
    output: Option<PathBuf>,
    /// Emit the complete run record as JSON on stdout.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct FieldArgs {
    /// Three increasing Gauss orders, separated by commas, for refinement.
    #[arg(long, value_delimiter = ',', num_args = 1, default_value = "6,10,14", value_parser = clap::value_parser!(u32).range(2..=24))]
    orders: Vec<u32>,
    /// Write a complete JSON report, including convergence and reference data.
    #[arg(long, short)]
    output: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct CoupledArgs {
    /// Exactly two increasing quadrature orders overriding the case's own
    /// declared numerics.quadrature_orders; omit to use the case's values.
    #[arg(long, value_delimiter = ',', num_args = 1, value_parser = clap::value_parser!(u32).range(2..=24))]
    orders: Vec<u32>,
    /// Write a complete JSON report, including per-tape/per-candidate detail.
    #[arg(long, short)]
    output: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

impl CoupledArgs {
    fn options(&self) -> Result<CoupledOptions, Box<dyn Error>> {
        let orders = if self.orders.is_empty() {
            None
        } else {
            Some(
                self.orders
                    .as_slice()
                    .try_into()
                    .map_err(|_| "provide exactly two comma-separated quadrature orders")?,
            )
        };
        Ok(CoupledOptions { orders })
    }
}

#[derive(Args)]
struct CoupledSearchArgs {
    /// May only lower the case's own declared execution.max_threads; omit to
    /// use the case's own value.
    #[arg(long)]
    threads: Option<u32>,
    /// Write a complete JSON record, including every candidate and the
    /// embedded acceptance recomputation.
    #[arg(long, short)]
    output: Option<PathBuf>,
    /// Supply a customer material dataset as a single-file bundle
    /// (optcoil-material-dataset/v1, see `dataset-bundle`). The case's
    /// declared material.dataset_id and material.csv_sha256 must match the
    /// bundle exactly — a mismatched dataset is rejected, never silently
    /// substituted.
    #[arg(long, conflicts_with_all = ["metadata", "csv"])]
    dataset_bundle: Option<PathBuf>,
    /// Customer dataset as a metadata/CSV pair (same validation as
    /// material-inspect). Both paths must be given together.
    #[arg(long, requires = "csv")]
    metadata: Option<PathBuf>,
    #[arg(long, requires = "metadata")]
    csv: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

impl CoupledSearchArgs {
    fn options(&self) -> CoupledSearchOptions {
        CoupledSearchOptions {
            threads: self.threads,
        }
    }

    fn dataset(&self) -> Result<Option<MaterialDataset>, Box<dyn Error>> {
        if let Some(bundle) = &self.dataset_bundle {
            return Ok(Some(MaterialDataset::from_bundle_json(
                &fs::read_to_string(bundle)?,
            )?));
        }
        if let (Some(metadata), Some(csv)) = (&self.metadata, &self.csv) {
            return Ok(Some(MaterialDataset::from_csv(
                &fs::read_to_string(metadata)?,
                &fs::read(csv)?,
            )?));
        }
        Ok(None)
    }

    fn refine_options(&self) -> CoupledRefineOptions {
        CoupledRefineOptions {
            threads: self.threads,
        }
    }
}

/// Bake-off args — a leaner CoupledSearchArgs: no --dataset-* flags (the
/// spec names every dataset source itself).
#[derive(Args)]
struct BakeoffArgs {
    /// May only lower the case's own declared execution.max_threads; omit to
    /// use the case's own value.
    #[arg(long)]
    threads: Option<u32>,
    /// Write a complete JSON record, including every entry's mutated case
    /// verbatim and the ranking.
    #[arg(long, short)]
    output: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ReportArgs {
    /// Save a complete new JSON report; existing files are protected.
    #[arg(long, short)]
    output: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct MaterialQueryArgs {
    #[arg(long)]
    temperature_k: f64,
    #[arg(long)]
    applied_field_t: f64,
    #[arg(long, allow_hyphen_values = true)]
    angle_from_normal_deg: f64,
    #[arg(long, default_value_t = 0.0001)]
    electric_field_criterion_v_per_m: f64,
    /// Selects an embedded dataset by id; ignored (and rejected) alongside
    /// --metadata/--csv, which name an explicit file pair instead. Never
    /// silently switches datasets.
    #[arg(long, default_value = "robinson-superpower-ap-v3", conflicts_with_all = ["metadata", "csv"])]
    dataset: String,
    /// Omit both paths to use the embedded dataset named by --dataset.
    #[arg(long, requires = "csv")]
    metadata: Option<PathBuf>,
    #[arg(long, requires = "metadata")]
    csv: Option<PathBuf>,
    #[command(flatten)]
    report: ReportArgs,
}

impl FieldArgs {
    fn options(&self) -> Result<FieldOptions, Box<dyn Error>> {
        Ok(FieldOptions {
            orders: self
                .orders
                .as_slice()
                .try_into()
                .map_err(|_| "provide exactly three comma-separated quadrature orders")?,
        })
    }
}

#[derive(Subcommand)]
enum DatasetCommand {
    /// Build a v2 dataset bundle from a metadata JSON + measurement CSV
    /// pair. The pair is fully validated before writing; the CSV text is
    /// embedded verbatim so the bundle pins the exact measured bytes.
    Build {
        metadata: PathBuf,
        csv: PathBuf,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Generate an issuer's ed25519 signing keypair. The output file
    /// contains the secret key — keep it private and out of version
    /// control; only the printed public key goes into a registry.
    Keygen {
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        key_id: String,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Sign a dataset bundle with an issuer key file. The attestation
    /// binds issuer + key_id + dataset_id + csv_sha256; existing
    /// attestation is replaced. Output is a v2 bundle.
    Attest {
        bundle: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Verify a bundle's schema, dataset binding and attestation, plus
    /// its registry status when --registry is supplied. Exit code is
    /// nonzero on any FAIL.
    Verify {
        bundle: PathBuf,
        /// Dataset registry supplying trusted issuer keys and entries.
        #[arg(long)]
        registry: Option<PathBuf>,
        /// Explicit `ed25519:<hex>` public key for registry-free signature
        /// checks — the caller vouches for the key.
        #[arg(long, conflicts_with = "registry")]
        pubkey: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Create or update a dataset registry: registers the bundle's
    /// dataset entry (countersigned by the registry key) and optionally
    /// an issuer's public key.
    RegistryUpsert {
        bundle: PathBuf,
        /// Registry file to create or update.
        #[arg(long)]
        registry: PathBuf,
        /// Registry signing key file (issuer must match the registry).
        #[arg(long)]
        key: PathBuf,
        #[arg(long, default_value = "current")]
        status: String,
        #[arg(long, default_value = "")]
        note: String,
        /// Also register this key file's public key under its issuer.
        #[arg(long)]
        add_key: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("OptCoil: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Command::Demo(args) => execute_run(Case::demo()?, args)?,
        Command::FieldBenchmark(args) => {
            let record = run_oc002(&args.options()?)?;
            finish_field(&record, &args)?;
        }
        Command::MaterialBenchmark(args) => {
            let record = run_material_suite()?;
            if let Some(path) = &args.output {
                record.write_new(path)?;
                eprintln!("Saved {}", path.display());
            }
            if args.json {
                println!("{}", serde_json::to_string_pretty(&record)?);
            } else {
                print_material_suite(&record);
            }
            if record.interpolation_validation_status != Status::Pass {
                return Err(format!(
                    "material interpolation validation is {:?}",
                    record.interpolation_validation_status
                )
                .into());
            }
        }
        Command::MaterialValidate {
            benchmark,
            dataset,
            metadata,
            csv,
            report,
        } => {
            let benchmark = MaterialBenchmark::from_json(&fs::read_to_string(benchmark)?)?;
            let material_dataset = if let (Some(metadata), Some(csv)) = (&metadata, &csv) {
                MaterialDataset::from_csv(&fs::read_to_string(metadata)?, &fs::read(csv)?)?
            } else {
                // No explicit file pair: default to the benchmark's own
                // declared dataset_id, matching it as an explicit choice
                // (not a silent switch) when --dataset overrides it.
                if let Some(requested) = &dataset
                    && requested != &benchmark.dataset_id
                {
                    return Err(format!(
                        "--dataset {requested} does not match this benchmark's declared dataset_id {}; refusing to silently switch datasets",
                        benchmark.dataset_id
                    )
                    .into());
                }
                let id = dataset.as_deref().unwrap_or(&benchmark.dataset_id);
                MaterialDataset::embedded_by_id(id)?
            };
            let record = run_material_benchmark(&material_dataset, &benchmark)?;
            if let Some(path) = &report.output {
                record.write_new(path)?;
                eprintln!("Saved {}", path.display());
            }
            if report.json {
                println!("{}", serde_json::to_string_pretty(&record)?);
            } else {
                print_material_run(&record);
            }
            if record.interpolation_validation_status != Status::Pass {
                return Err(format!(
                    "material validation is {:?}",
                    record.interpolation_validation_status
                )
                .into());
            }
        }
        Command::MaterialQuery(args) => execute_material_query(args)?,
        Command::MaterialInspect {
            metadata,
            csv,
            json,
        } => {
            let data = MaterialDataset::from_csv(&fs::read_to_string(metadata)?, &fs::read(csv)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                println!(
                    "{} / {} / MEASURED",
                    data.metadata.id, data.metadata.material
                );
                println!(
                    "{} measurements; sample {}; measured bridge width {:.3} mm",
                    data.points.len(),
                    data.metadata.sample_id,
                    data.metadata.measured_bridge_width_m * 1000.0
                );
                println!(
                    "Nominal temperature levels: {:?} K; applied-field levels: {:?} T",
                    data.metadata.selection.nominal_temperature_k,
                    data.metadata.selection.nominal_field_t
                );
                println!(
                    "Angle selection {:?} degrees from tape normal; actual measured coordinates determine interpolation coverage",
                    data.metadata.selection.nominal_angle_range_deg
                );
                println!(
                    "Source: {}; license {}",
                    data.metadata.source_doi, data.metadata.license
                );
                println!(
                    "CSV SHA-256 verified. Current is A/m of measured bridge width, not a full-tape engineering allowance."
                );
            }
        }
        Command::Dataset { command } => execute_dataset(command)?,
        Command::DatasetBundle {
            metadata,
            csv,
            output,
        } => {
            let metadata_json = fs::read_to_string(metadata)?;
            let csv_bytes = fs::read(csv)?;
            // Validate the pair before bundling: the bundle is only as
            // trustworthy as the metadata.csv_sha256 binding inside it.
            let dataset = MaterialDataset::from_csv(&metadata_json, &csv_bytes)?;
            let csv_data = String::from_utf8(csv_bytes)
                .map_err(|_| "material CSV is not UTF-8 and cannot be bundled")?;
            let bundle = serde_json::json!({
                "schema": "optcoil-material-dataset/v1",
                "metadata": serde_json::from_str::<serde_json::Value>(&metadata_json)?,
                "csv_data": csv_data,
            });
            let text = serde_json::to_string_pretty(&bundle)?;
            // Round-trip check: the written bundle must re-validate.
            MaterialDataset::from_bundle_json(&text)?;
            let mut file = fs::File::options()
                .write(true)
                .create_new(true)
                .open(&output)
                .map_err(|e| format!("cannot create {}: {e}", output.display()))?;
            use std::io::Write as _;
            file.write_all(text.as_bytes())?;
            eprintln!(
                "Saved dataset bundle {} ({} / {} measurements, csv sha {})",
                output.display(),
                dataset.metadata.id,
                dataset.points.len(),
                &dataset.metadata.csv_sha256[..16],
            );
        }
        Command::Field {
            case,
            reference,
            args,
        } => {
            let json = fs::read_to_string(case)?;
            let reference = reference.map(fs::read_to_string).transpose()?;
            let record = run_field_case(&json, reference.as_deref(), &args.options()?)?;
            finish_field(&record, &args)?;
        }
        Command::CoupledBenchmark(args) => {
            let record = run_oc004(&args.options()?)?;
            finish_coupled(&record, &args)?;
        }
        Command::Coupled {
            case,
            reference,
            args,
        } => {
            let json = fs::read_to_string(case)?;
            let reference = reference.map(fs::read_to_string).transpose()?;
            let record = run_coupled_case(&json, reference.as_deref(), &args.options()?)?;
            finish_coupled(&record, &args)?;
        }
        Command::CoupledSearchBenchmark(args) => {
            let record = run_oc007(&args.options())?;
            finish_coupled_search(&record, &args)?;
        }
        Command::CoupledSearch { case, args } => {
            let json = fs::read_to_string(case)?;
            let dataset = args.dataset()?;
            let parsed = optcoil_model::coupled_search::CoupledSearchCase::from_json(&json)?;
            let mut dataset_map = std::collections::BTreeMap::new();
            if let Some(dataset) = &dataset {
                dataset_map.insert(dataset.metadata.id.clone(), dataset.clone());
            }
            let preflight = optcoil_search::preflight::preflight_coupled_search_with_options(
                &parsed,
                Some(&dataset_map),
                &args.options(),
            );
            if !preflight.ready_to_run {
                return Err(preflight
                    .errors
                    .iter()
                    .map(|item| {
                        format!(
                            "{} {}",
                            item.message,
                            item.correction.as_deref().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into());
            }
            eprintln!(
                "Preflight: {} candidates; {} primary sampled points (estimate), q {:?}. Input readiness does not establish coverage.",
                preflight.workload.candidate_count,
                preflight.workload.primary_point_upper_estimate,
                preflight.quadrature_orders
            );
            // Long grids print a progress line every 2 s on stderr —
            // silent until enumeration lands, and quiet for fast cases.
            // Progress is a view concern only; verdicts/ledger unchanged.
            let progress = Arc::new(SearchProgress::new());
            let done_flag = Arc::new(AtomicBool::new(false));
            let reporter = {
                let progress = Arc::clone(&progress);
                let done_flag = Arc::clone(&done_flag);
                thread::spawn(move || {
                    while !done_flag.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_secs(2));
                        if done_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        let (fraction, done, planned) = progress.fraction();
                        let n_done = progress.candidates_done.load(Ordering::Relaxed);
                        let n_total = progress.candidates_total.load(Ordering::Relaxed);
                        if n_total == 0 {
                            continue;
                        }
                        if planned > 0 {
                            eprintln!(
                                "{n_done}/{n_total} candidates · {done}/{planned} field evals ({:.0}%) · {}",
                                fraction * 100.0,
                                progress.phase_label(),
                            );
                        } else {
                            eprintln!("{n_done}/{n_total} candidates · {}", progress.phase_label(),);
                        }
                    }
                })
            };
            let record = run_coupled_search_case_with_dataset_progress(
                &json,
                &args.options(),
                dataset,
                &AtomicBool::new(false),
                Some(progress.as_ref()),
            );
            done_flag.store(true, Ordering::Relaxed);
            let _ = reporter.join();
            let record = record?;
            finish_coupled_search(&record, &args)?;
        }
        Command::Preflight { case, args } => {
            let case = optcoil_model::coupled_search::CoupledSearchCase::from_json(
                &fs::read_to_string(case)?,
            )?;
            let mut datasets = std::collections::BTreeMap::new();
            if let Some(dataset) = args.dataset()? {
                datasets.insert(dataset.metadata.id.clone(), dataset);
            }
            let preflight = optcoil_search::preflight::preflight_coupled_search_with_options(
                &case,
                Some(&datasets),
                &args.options(),
            );
            if args.json {
                println!("{}", serde_json::to_string_pretty(&preflight)?);
            } else {
                println!(
                    "{}: {} candidates · {} primary sampled points (estimate) · up to {} threads · q {:?}",
                    preflight.case_id,
                    preflight.workload.candidate_count,
                    preflight.workload.primary_point_upper_estimate,
                    preflight.selected_threads,
                    preflight.quadrature_orders
                );
                println!(
                    "{} · {}",
                    preflight.geometry_support, preflight.field_source
                );
                for item in &preflight.items {
                    println!("{:?}: {}", item.level, item.message);
                    if let Some(correction) = &item.correction {
                        println!("  {correction}");
                    }
                }
                println!(
                    "Readiness checks inputs; coverage and engineering acceptance require evaluation."
                );
            }
            if !preflight.ready_to_run {
                return Err("preflight found incompatible or missing inputs".into());
            }
        }
        Command::ReviewPackage {
            record,
            case,
            output,
            archive,
            dataset_bundle,
        } => {
            let record_json = fs::read_to_string(record)?;
            let case_json = fs::read_to_string(case)?;
            let bundles = dataset_bundle
                .iter()
                .map(|path| {
                    MaterialBundle::from_json(&fs::read_to_string(path)?)
                        .map_err(Box::<dyn Error>::from)
                })
                .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
            let artifacts =
                optcoil_search::review::review_package(&record_json, Some(&case_json), &bundles)?;
            if archive {
                use std::io::Write;
                let bytes = optcoil_search::review::review_package_tar(&artifacts)?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output)?;
                file.write_all(&bytes)?;
            } else {
                write_review_package(&output, &artifacts)?;
            }
            println!(
                "Saved {} artifacts to {}",
                artifacts.len(),
                output.display()
            );
        }
        Command::VerifyPackage { directory } => {
            let manifest_json = fs::read_to_string(directory.join("manifest.json"))?;
            let manifest: optcoil_search::review::ReviewPackageManifest =
                serde_json::from_str(&manifest_json)?;
            let mut artifacts = vec![("manifest.json".into(), manifest_json)];
            for path in manifest.files.keys() {
                // Verify paths before reading, so a crafted manifest cannot escape the package.
                if Path::new(path)
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
                {
                    return Err(format!("unsafe package path: {path}").into());
                }
                artifacts.push((path.clone(), fs::read_to_string(directory.join(path))?));
            }
            optcoil_search::review::verify_review_package(&artifacts)?;
            println!(
                "PASS: artifact hashes, case/dataset bindings and ledger arithmetic; physics validation remains separate."
            );
        }
        Command::Sensitivity { case, spec, args } => {
            let json = fs::read_to_string(case)?;
            let spec_json = fs::read_to_string(spec)?;
            let record = optcoil_search::sensitivity::run_sensitivity_sweep(
                &json,
                &spec_json,
                &args.options(),
                args.dataset()?,
                &std::sync::atomic::AtomicBool::new(false),
            )?;
            if let Some(path) = &args.output {
                record.write_new(path)?;
                eprintln!("Saved {}", path.display());
            }
            if args.json {
                println!("{}", serde_json::to_string_pretty(&record)?);
            } else {
                for point in &record.points {
                    let axes = point
                        .values
                        .iter()
                        .map(|v| format!("{}={}", v.axis, v.value))
                        .collect::<Vec<_>>()
                        .join(" ");
                    match (&point.optimum, point.error.as_ref()) {
                        (_, Some(e)) => println!("{axes:<40} ERROR {e}"),
                        (Some(g), None) => println!(
                            "{axes:<40} {}x{} -> {:?}/{:?} · saves {}",
                            g.turns_along_normal,
                            g.tapes_along_width,
                            point.search_status,
                            point.agreement_status,
                            point
                                .savings_usd
                                .map(|s| format!("${s:.0}"))
                                .unwrap_or_else(|| "n/a".into())
                        ),
                        (None, None) => println!(
                            "{axes:<40} no PASS optimum · {:?}/{:?}",
                            point.search_status, point.agreement_status
                        ),
                    }
                }
            }
            if record.points.iter().any(|p| p.error.is_some()) {
                return Err("sensitivity sweep had invalid points".into());
            }
        }
        Command::Bakeoff { case, spec, args } => {
            let json = fs::read_to_string(case)?;
            let spec_json = fs::read_to_string(&spec)?;
            let bundle_root = spec.parent().unwrap_or_else(|| std::path::Path::new("."));
            let record = optcoil_search::bakeoff::run_bakeoff(
                &json,
                &spec_json,
                &optcoil_search::coupled_search::CoupledSearchOptions {
                    threads: args.threads,
                },
                bundle_root,
                &std::sync::atomic::AtomicBool::new(false),
            )?;
            if let Some(path) = &args.output {
                record.write_new(path)?;
                eprintln!("Saved {}", path.display());
            }
            if args.json {
                println!("{}", serde_json::to_string_pretty(&record)?);
            } else {
                for entry in &record.entries {
                    let label = entry.product_id.as_deref().unwrap_or(&entry.dataset_id);
                    match (&entry.optimum, &entry.error) {
                        (_, Some(e)) => println!("{:<44} ERROR {e}", label),
                        (Some(g), None) => println!(
                            "{:<44} {:?}/{:?} · {}x{} · {} · saves {} · util {}",
                            label,
                            entry.search_status,
                            entry.agreement_status,
                            g.turns_along_normal,
                            g.tapes_along_width,
                            entry
                                .optimum_total_usd
                                .map(|t| format!("total ${t:.0}"))
                                .unwrap_or_else(|| "n/a".into()),
                            entry
                                .savings_usd
                                .map(|s| format!("${s:.0}"))
                                .unwrap_or_else(|| "n/a".into()),
                            entry
                                .optimum_max_utilization
                                .map(|u| format!("{u:.2}"))
                                .unwrap_or_else(|| "n/a".into())
                        ),
                        (None, None) => println!(
                            "{:<44} {:?}/{:?} · no PASS optimum",
                            label, entry.search_status, entry.agreement_status
                        ),
                    }
                }
                println!("ranking: {}", record.ranking.join(" > "));
            }
        }
        Command::GradeReport {
            record,
            output,
            json,
        } => {
            let record_json = fs::read_to_string(record)?;
            let report = optcoil_search::gradereport::grade_report_from_record(&record_json)?;
            if let Some(path) = &output {
                report.write_new(path)?;
                eprintln!("Saved {}", path.display());
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for row in &report.uniform_rows {
                    println!(
                        "uniform {:<20} {:<40} ${:.2} · saves {} · util {}",
                        row.spec_id,
                        row.dataset_id,
                        row.total_usd,
                        row.savings_usd
                            .map(|s| format!("${s:.0}"))
                            .unwrap_or_else(|| "n/a".into()),
                        row.max_utilization
                            .map(|u| format!("{u:.2}"))
                            .unwrap_or_else(|| "n/a".into())
                    );
                }
                println!(
                    "optimum: {} · ${:.2}",
                    report
                        .optimum_assignment
                        .map(|a| a.join("+"))
                        .unwrap_or_else(|| "none".into()),
                    report.optimum_total_usd.unwrap_or(f64::NAN)
                );
                println!(
                    "grading delta vs best uniform ({}): {}",
                    report.best_uniform_spec_id.as_deref().unwrap_or("n/a"),
                    report
                        .grading_delta_usd
                        .map(|d| format!(
                            "${d:.2} ({:.1}%)",
                            report.grading_delta_percent.unwrap_or(0.0)
                        ))
                        .unwrap_or_else(|| "n/a".into())
                );
            }
        }
        Command::Bom {
            record,
            output,
            rfq,
            json,
        } => {
            let record_json = fs::read_to_string(record)?;
            let bom = optcoil_search::bom::bom_from_record(&record_json)?;
            if let Some(path) = &output {
                bom.write_new(path)?;
                eprintln!("Saved {}", path.display());
            }
            if let Some(path) = &rfq {
                write_new(
                    path,
                    &optcoil_search::bom::rfq_markdown_from_record(&record_json)?,
                )?;
                eprintln!("Saved {}", path.display());
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&bom)?);
            } else {
                for row in &bom.spec_rows {
                    let ranges: Vec<String> = row
                        .turn_ranges
                        .iter()
                        .map(|r| format!("{}-{}", r[0], r[1]))
                        .collect();
                    println!(
                        "{:<20} turns {:<20} {:.1} m installed · {:.1} m buy @ ${}/m · ${:.0}",
                        row.spec_id,
                        ranges.join(","),
                        row.installed_length_m,
                        row.purchased_length_m,
                        row.price_usd_per_m,
                        row.conductor_usd + row.scrap_usd
                    );
                    if let (Some(pieces), Some(len)) = (row.pieces, row.piece_length_m) {
                        println!(
                            "  pieces {} x {:.1} m · splices {} · remnant {:.1} m",
                            pieces,
                            len,
                            row.piece_splices.unwrap_or(0),
                            row.remnant_length_m.unwrap_or(0.0)
                        );
                    }
                }
                match (bom.module_joints, bom.piece_splices, bom.spec_splices) {
                    (Some(m), Some(p), Some(s)) => println!(
                        "joints {} (${:.0}) = {} module + {} piece splices + {} spec splices · pancakes {} (${:.0})",
                        bom.joint_count,
                        bom.joints_usd,
                        m,
                        p,
                        s,
                        bom.pancake_count,
                        bom.assembly_usd
                    ),
                    _ => println!(
                        "joints {} (${:.0}) · pancakes {} (${:.0})",
                        bom.joint_count, bom.joints_usd, bom.pancake_count, bom.assembly_usd
                    ),
                }
                println!(
                    "total ${:.2} · installed {:.1} m · totals agree: {}",
                    bom.total_usd, bom.total_installed_length_m, bom.totals_agree
                );
            }
        }
        Command::CoupledRefineBenchmark(args) => {
            let record = run_oc008(&args.refine_options())?;
            finish_coupled_refine(&record, &args)?;
        }
        Command::CoupledRefine { case, args } => {
            let json = fs::read_to_string(case)?;
            let record = optcoil_search::coupled_refine::run_coupled_refine_case_with_dataset(
                &json,
                &args.refine_options(),
                args.dataset()?,
            )?;
            finish_coupled_refine(&record, &args)?;
        }
        Command::Reprice {
            record,
            price_usd_per_m,
            price_provenance,
            output,
        } => {
            let json = fs::read_to_string(&record)?;
            let sha = optcoil_search::reprice::record_sha256(&json);
            let note = optcoil_search::reprice::reprice_record(
                &json,
                &sha,
                price_usd_per_m,
                &price_provenance,
            )?;
            let base = &note.candidates[note.baseline_index];
            println!(
                "Baseline {} -> ${:.0}",
                geometry_label_cli(&base.geometry),
                base.total_usd
            );
            match note.best_index {
                Some(i) => {
                    let best = &note.candidates[i];
                    println!(
                        "Optimum  {} -> ${:.0} (was ${:.0} at ${}/m)",
                        geometry_label_cli(&best.geometry),
                        best.total_usd,
                        record_optimum_total(&json),
                        note.source_price_usd_per_m
                    );
                    println!(
                        "Savings  ${:.0} ({:.1}%)  ·  source search {:?} · source acceptance {:?} · scenario acceptance NOT_EVALUATED",
                        note.savings_usd.unwrap_or(0.0),
                        note.savings_percent.unwrap_or(0.0),
                        note.source_search_status,
                        note.source_acceptance_agreement_status
                    );
                }
                None => println!("Optimum  none (no PASS candidate in the source record)"),
            }
            if let Some(path) = output {
                note.write_new(&path)?;
                println!("Saved {}", path.display());
            }
        }
        Command::Verify {
            record,
            case,
            manifest,
            dataset,
        } => {
            let record_json = fs::read_to_string(&record)?;
            let case_json = case.map(fs::read_to_string).transpose()?;
            let manifest_json = manifest.as_ref().map(fs::read_to_string).transpose()?;
            let manifest_dir = manifest.as_ref().and_then(|p| p.parent().map(Path::new));
            let dataset_jsons: Vec<String> = dataset
                .iter()
                .map(fs::read_to_string)
                .collect::<Result<_, _>>()?;
            let dataset_refs: Vec<&str> = dataset_jsons.iter().map(String::as_str).collect();
            optcoil_search::verify::verify_record(
                &record_json,
                case_json.as_deref(),
                manifest_json.as_deref(),
                manifest_dir,
                &dataset_refs,
            )?;
        }
        Command::Report { record, output } => {
            let record: CoupledSearchRunRecord =
                serde_json::from_str(&fs::read_to_string(record)?)?;
            let html = optcoil_search::report::render_search_report_html(&record)?;
            let mut file = fs::File::options()
                .write(true)
                .create_new(true)
                .open(&output)
                .map_err(|e| format!("cannot create {}: {e}", output.display()))?;
            use std::io::Write as _;
            file.write_all(html.as_bytes())?;
            eprintln!("Saved {}", output.display());
        }
        Command::FieldMap {
            case,
            emit_case,
            source,
            spacing_m,
            margin_m,
            samples,
            reference_ni,
        } => {
            let text = fs::read_to_string(&case)?;
            let mut case = optcoil_model::coupled_search::CoupledSearchCase::from_json(&text)?;
            let Some(path3d) = case.fixed_geometry.path3d.clone() else {
                return Err(
                    "field-map generation applies to path3d cases — this case's \
                            fixed_geometry declares no path3d"
                        .into(),
                );
            };
            let (Some(radial_band), Some(width_band)) = (
                case.fixed_geometry.pack_radial_width_m,
                case.fixed_geometry.pack_axial_height_m,
            ) else {
                return Err("path3d cases must declare \
                        pack_radial_width_m/pack_axial_height_m"
                    .into());
            };
            let generated = optcoil_search::fieldmap::generate_path3d_field_map(
                &path3d,
                radial_band,
                width_band,
                case.requirement.bore_probe_m,
                reference_ni,
                &optcoil_search::fieldmap::MapGrid {
                    spacing_m,
                    margin_m,
                    line_samples: samples,
                    ..Default::default()
                },
            )
            .map_err(|e| format!("field-map generation failed: {e}"))?;
            if source.exists() || emit_case.exists() {
                return Err("refusing to overwrite an existing file".into());
            }
            fs::write(&source, &generated.source_csv)?;
            case.field_map = Some(optcoil_search::fieldmap::declared_field_map(&generated));
            // The injected map has to pass the schema's own gates
            // (product-grid completeness, hull containment, anchor
            // finiteness) — validate the assembled case before writing
            // so a bad grid can't ship as a declared case.
            case.validate()
                .map_err(|e| format!("generated case fails validation: {e}"))?;
            let json = serde_json::to_string_pretty(&case)?;
            fs::write(&emit_case, format!("{json}\n"))?;
            if let optcoil_model::coupled::FieldMap::CartesianBxByBz {
                x_levels_m,
                y_levels_m,
                z_levels_m,
                entries,
                ..
            } = &generated.map
            {
                println!(
                    "map: {}×{}×{} = {} nodes, source sha256={}",
                    x_levels_m.len(),
                    y_levels_m.len(),
                    z_levels_m.len(),
                    entries.len(),
                    generated.source_sha256
                );
            }
            println!(
                "bore anchor: {:.6} T at {:?} (NI = {})",
                generated.bore_field_at_reference_t, case.requirement.bore_probe_m, reference_ni
            );
            println!(
                "wrote {} and {} — filament-model map; declare its origin \
                 in case provenance",
                emit_case.display(),
                source.display()
            );
        }
        Command::Run { case, args } => {
            execute_run(Case::from_json(&fs::read_to_string(case)?)?, args)?
        }
        Command::Inspect { case, json } => {
            let case = Case::from_json(&fs::read_to_string(case)?)?;
            let assessment = acceptance::assess(&case, &case.baseline)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&assessment)?);
            } else {
                println!("{} — SYNTHETIC\n{}", case.id, case.description);
                println!(
                    "{} modules, {} grades",
                    case.modules.len(),
                    case.grades.len()
                );
                println!(
                    "Baseline: ${:.2}; screening {:?}; engineering {:?}",
                    assessment.cost.total_usd,
                    assessment.screening_status,
                    assessment.engineering_status
                );
                for check in assessment.checks {
                    println!("  {:?} {}: {}", check.status, check.id, check.detail);
                }
            }
        }
        Command::Crosscheck {
            deck,
            record,
            evaluation_model,
            order,
            python,
            json,
        } => {
            let deck_json = if let Some(path) = deck {
                fs::read_to_string(path)?
            } else if let Some(path) = record {
                let record_json = fs::read_to_string(path)?;
                deck_json_from_search_record(&record_json, &evaluation_model)?
            } else {
                return Err("crosscheck needs --deck <file> or --record <file>".into());
            };
            let mut crosscheck = BluemiraCrosscheck::default();
            if let Some(python) = python {
                crosscheck.python = python;
            }
            let run = run_kernel_crosscheck(&deck_json, &crosscheck, order)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&run)?);
            } else {
                let v = &run.verdict;
                println!(
                    "kernel crosscheck: {} — {}",
                    v.status.to_uppercase(),
                    v.basis
                );
                if let (Some(rel), Some(dir)) = (v.max_magnitude_rel_error, v.max_direction_deg) {
                    println!(
                        "  max |dB|/B: {:.3e} over {} probes; max direction delta {:.3} deg",
                        rel, v.probes_compared, dir
                    );
                }
                if let Some(ev) = &run.evidence {
                    println!(
                        "  {} {} — request {} response {}",
                        ev.response.solver_name,
                        ev.response.solver_version,
                        &ev.request_sha256[..16],
                        &ev.response_sha256[..16]
                    );
                }
            }
            // A check command exits nonzero on anything but pass — an
            // inconclusive or unevaluated cross-check is not "verified".
            if run.verdict.status != "pass" {
                return Err(format!("kernel crosscheck verdict: {}", run.verdict.status).into());
            }
        }
        Command::Adapters => {
            for adapter in PLANNED_ADAPTERS {
                let d = adapter.descriptor();
                println!(
                    "{}: {:?} — {}",
                    d.name, d.availability, d.integration_surface
                );
            }
            println!("No external solver is implemented or invoked by this scaffold.");
        }
    }
    Ok(())
}

/// Build an OC-011 probe deck from a coupled-search run record: the
/// selected candidate's pack geometry and solved NI, probed at the
/// region lattice plus the bare bore probe, under the requested
/// evaluation model (thin_filament Phase A, or finite_cross_section
/// Phase B — the model the search verdicts run on).
fn deck_json_from_search_record(
    record_json: &str,
    evaluation_model: &str,
) -> Result<String, Box<dyn Error>> {
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)?;
    let best_index = record
        .best_index
        .ok_or("record has no best candidate — nothing to cross-check")?;
    let best = &record.candidates[best_index];
    let case = &record.case;
    let mut probes = vec![case.requirement.bore_probe_m];
    if let Some(region) = &case.requirement.good_field_region {
        probes.extend(region.lattice_points(case.requirement.bore_probe_m));
    }
    if case.fixed_geometry.path.is_some() {
        return Err(
            "kernel probe deck is racetrack-shaped (optcoil-kernel-probe/v1); \
             path-geometry records have no deck format yet"
                .into(),
        );
    }
    let deck = KernelProbeRequest {
        schema: KERNEL_PROBE_DECK_SCHEMA.to_string(),
        geometry: ProbeGeometry {
            straight_half_length_m: case
                .fixed_geometry
                .straight_half_length_m
                .ok_or("record declares neither path nor racetrack dimensions")?,
            // `bend_radius_m` is the pack's midline radius — the filament
            // centerline.
            centerline_bend_radius_m: case
                .fixed_geometry
                .bend_radius_m
                .ok_or("record declares neither path nor racetrack dimensions")?,
            radial_width_m: best.geometry.radial_width_m,
            axial_height_m: best.geometry.axial_height_m,
            tape_normal: format!("{:?}", case.fixed_geometry.tape_normal).to_lowercase(),
        },
        evaluation_model: evaluation_model.to_string(),
        ampere_turns_a: best.ampere_turns_a,
        probes_m: probes,
    };
    Ok(serde_json::to_string(&deck)?)
}

fn execute_run(case: Case, args: RunArgs) -> Result<(), Box<dyn Error>> {
    let record = run(
        &case,
        &SearchOptions {
            max_evaluations: args.max_evaluations,
        },
    )?;
    if let Some(path) = &args.output {
        record.write_new(path)?;
        eprintln!("Saved {}", path.display());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else {
        print_summary(&record);
    }
    Ok(())
}

fn print_summary(record: &RunRecord) {
    println!("OptCoil / {} / SYNTHETIC BENCHMARK", record.case.id);
    println!(
        "Baseline ${:.2} -> candidate ${:.2}; modeled saving ${:.2} ({:.2}%)",
        record.baseline.cost.total_usd,
        record.best.cost.total_usd,
        record.savings_usd,
        record.savings_percent
    );
    println!(
        "Search {:?}: {}/{} candidates; model optimum: {}",
        record.termination,
        record.evaluated_candidates,
        record.search_space_size,
        record.proven_optimal_in_screening_model
    );
    for allocation in &record.best.candidate.allocations {
        println!(
            "  {}: {} x {} tape",
            allocation.module_id, allocation.tapes, allocation.grade_id
        );
    }
    println!(
        "Screening {:?}; engineering {:?}",
        record.best.screening_status, record.best.engineering_status
    );
    println!(
        "Synthetic allocation only. Coupled fields, mechanics, thermal and quench are NOT_EVALUATED."
    );
}

fn print_material_suite(record: &MaterialSuiteRecord) {
    println!("OptCoil — measured conductor interpolation");
    for result in [
        &record.linear_baseline,
        &record.logarithmic_development,
        &record.reserved_challenge,
    ] {
        println!(
            "{}: {:?} / {:.1} ms",
            result.benchmark.id, result.interpolation_validation_status, result.elapsed_ms
        );
        for fold in &result.folds {
            println!(
                "  {}: {}/{} covered; P95 {:.3}%, max {:.3}%, max overprediction {:.3}%",
                fold.fold.id,
                fold.covered_points,
                fold.points.len(),
                fold.p95_relative_error.unwrap_or(f64::NAN) * 100.0,
                fold.max_relative_error.unwrap_or(f64::NAN) * 100.0,
                fold.max_positive_relative_error.unwrap_or(f64::NAN) * 100.0
            );
        }
    }
    println!(
        "Logarithmic model validation {:?}; engineering {:?}",
        record.interpolation_validation_status, record.engineering_status
    );
    println!(
        "Single 1 mm research bridge; applied-field data. No full-width tape qualification or coupling to the coil field is established."
    );
}

fn print_material_run(record: &MaterialRunRecord) {
    println!(
        "OptCoil / {} / MEASURED MATERIAL VALIDATION",
        record.benchmark.id
    );
    println!(
        "{:?} / {:.1} ms",
        record.interpolation_validation_status, record.elapsed_ms
    );
    for fold in &record.folds {
        println!(
            "  {}: {}/{} covered; P95 {:.3}%, max {:.3}%, max overprediction {:.3}%; status {:?}",
            fold.fold.id,
            fold.covered_points,
            fold.points.len(),
            fold.p95_relative_error.unwrap_or(f64::NAN) * 100.0,
            fold.max_relative_error.unwrap_or(f64::NAN) * 100.0,
            fold.max_positive_relative_error.unwrap_or(f64::NAN) * 100.0,
            fold.status
        );
        if !fold.strata.is_empty() {
            println!(
                "    off-grid axes | points | covered | coverage  |    P95   |   worst  | worst positive"
            );
            for s in &fold.strata {
                println!(
                    "    {:>13} | {:>6} | {:>7} | {:>7.3}% | {:>6.3}% | {:>6.3}% | {:>7.3}%",
                    s.off_grid_axes,
                    s.points,
                    s.covered_points,
                    s.coverage_fraction * 100.0,
                    s.p95_relative_error.unwrap_or(f64::NAN) * 100.0,
                    s.max_relative_error.unwrap_or(f64::NAN) * 100.0,
                    s.max_positive_relative_error.unwrap_or(f64::NAN) * 100.0
                );
            }
            if let Some(gate) = &record.benchmark.stratum_acceptance {
                let three_axis = fold.strata.iter().find(|s| s.off_grid_axes == 3);
                let observed = three_axis
                    .map(|s| {
                        if s.points == 0 {
                            "empty (INCONCLUSIVE)".to_string()
                        } else {
                            format!(
                                "{} points, worst positive {}",
                                s.points,
                                s.max_positive_relative_error
                                    .map(|v| format!("{:.3}%", v * 100.0))
                                    .unwrap_or_else(|| "n/a (INCONCLUSIVE)".into())
                            )
                        }
                    })
                    .unwrap_or_else(|| "no three-axis stratum".into());
                println!(
                    "    three-axis stratum gate {:.3}%: {observed}",
                    gate.three_axis_max_positive_relative_error * 100.0
                );
            }
        }
    }
    println!(
        "Single 1 mm research bridge; applied-field data. No full-width tape qualification or coupling to the coil field is established."
    );
}

fn execute_material_query(args: MaterialQueryArgs) -> Result<(), Box<dyn Error>> {
    let query = MaterialQuery {
        temperature_k: args.temperature_k,
        applied_field_t: args.applied_field_t,
        angle_from_normal_deg: args.angle_from_normal_deg,
        electric_field_criterion_v_per_m: args.electric_field_criterion_v_per_m,
    };
    let record = if let (Some(metadata), Some(csv)) = (&args.metadata, &args.csv) {
        query_material(&fs::read_to_string(metadata)?, &fs::read(csv)?, query)?
    } else {
        query_embedded_material_by_id(&args.dataset, query)?
    };
    if let Some(path) = &args.report.output {
        record.write_new(path)?;
        eprintln!("Saved {}", path.display());
    }
    if args.report.json {
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else {
        println!(
            "OptCoil / {} / MEASURED MATERIAL QUERY",
            record.dataset.metadata.id
        );
        println!(
            "{:.3} K, {:.4} T applied, {:.3} degrees from tape normal",
            query.temperature_k, query.applied_field_t, query.angle_from_normal_deg
        );
        if let Some(value) = &record.estimate {
            println!(
                "Ic per width: {:.3} A/m; equivalent current for the measured {:.3} mm bridge: {:.3} A",
                value.ic_a_per_m,
                record.dataset.metadata.measured_bridge_width_m * 1000.0,
                value.ic_a_per_m * record.dataset.metadata.measured_bridge_width_m
            );
            println!(
                "Source rows and weights: {}",
                value
                    .support
                    .iter()
                    .map(|s| format!("{} ({:.4})", s.source_row, s.weight))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!(
            "Domain {:?}; criterion {:?}; engineering {:?}",
            record.domain_status, record.criterion_status, record.engineering_status
        );
        println!("{}", record.detail);
        println!(
            "Source: {}; {}",
            record.dataset.metadata.source_doi, record.dataset.metadata.license
        );
    }
    if record.domain_status != Status::Pass || record.criterion_status != Status::Pass {
        return Err(
            "material query is INCONCLUSIVE; no supported current estimate was produced".into(),
        );
    }
    Ok(())
}

fn finish_field(record: &FieldRunRecord, args: &FieldArgs) -> Result<(), Box<dyn Error>> {
    if let Some(path) = &args.output {
        record.write_new(path)?;
        eprintln!("Saved {}", path.display());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(record)?);
    } else {
        println!(
            "OptCoil / {} / PRESCRIBED-CURRENT FIELD BENCHMARK",
            record.case.id
        );
        println!(
            "{} probes, {} source cells, orders {:?}; {:.1} ms total (single thread)",
            record.results.len(),
            record.source_cells,
            record.options.orders,
            record.elapsed_ms
        );
        println!(
            "Numerical {:?}; engineering {:?}",
            record.numerical_status, record.engineering_status
        );
        if let (Some(error), Some(fraction)) = (
            record.max_reference_error_t,
            record.max_reference_error_fraction,
        ) {
            println!(
                "Maximum reference discrepancy: {error:.6e} T ({:.6}% of fixed {:.3} T scale)",
                100.0 * fraction,
                record.case.acceptance.field_scale_t
            );
        }
        println!(
            "Maximum last-order change: {:.6e} T ({:.6}% of field scale)",
            record.max_refinement_change_t,
            record.max_refinement_change_fraction * 100.0
        );
        if let Some(change) = record.max_reference_refinement_t {
            println!(
                "Reference's last-order change: {change:.6e} T (observed convergence, not an error bound)"
            );
        }
        for probe in &record.results {
            let b = probe.levels[2].field_t;
            println!(
                "  {:22} B = [{:+.6e}, {:+.6e}, {:+.6e}] T",
                probe.id, b[0], b[1], b[2]
            );
        }
        for check in &record.checks {
            println!("  {:?}: {}", check.status, check.id);
        }
        println!(
            "Uniform winding-pack current only. HTS response, mechanics, thermal, quench and cost coupling are NOT_EVALUATED."
        );
    }
    if record.numerical_status != Status::Pass {
        return Err(format!(
            "field validation is {:?}; the report remains available for diagnosis",
            record.numerical_status
        )
        .into());
    }
    Ok(())
}

fn finish_coupled(record: &CoupledRunRecord, args: &CoupledArgs) -> Result<(), Box<dyn Error>> {
    if let Some(path) = &args.output {
        record.write_new(path)?;
        eprintln!("Saved {}", path.display());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(record)?);
    } else {
        print_coupled(record);
    }
    let all_candidates_pass = record.candidates.iter().all(|c| c.status == Status::Pass);
    if record.numerical_status != Status::Pass
        || record.coverage_status != Status::Pass
        || !all_candidates_pass
    {
        return Err(format!(
            "coupled screening is not fully PASS (numerical {:?}, coverage {:?}, all candidates PASS: {all_candidates_pass}); the report remains available for diagnosis",
            record.numerical_status, record.coverage_status
        )
        .into());
    }
    Ok(())
}

fn print_coupled(record: &CoupledRunRecord) {
    println!(
        "OptCoil / {} / COUPLED CONDUCTOR-GEOMETRY SCREENING",
        record.case.id
    );
    println!(
        "Dataset {}; total turns {}; {:.1} ms total (single thread, {} kernel evaluations)",
        record.dataset.id, record.total_turns, record.elapsed_ms, record.kernel_evaluations
    );
    for candidate in &record.candidates {
        let limiting = candidate
            .limiting
            .as_ref()
            .map(|l| {
                format!(
                    "{}/tape{}/turn{}/w{}",
                    l.station, l.tape_index, l.turn_index, l.width_index
                )
            })
            .unwrap_or_else(|| "none (coverage incomplete)".into());
        println!(
            "  I={:.3} A ({:.3} A-turn): {:?}; limiting {limiting}; min allowed {:.3} A; max utilization {:.4}",
            candidate.current_a,
            candidate.ampere_turns_a,
            candidate.status,
            candidate.min_allowed_screening_a.unwrap_or(f64::NAN),
            candidate.max_utilization.unwrap_or(f64::NAN),
        );
        println!(
            "    points estimate/lower_bound/unsupported/along_current_excluded: {}/{}/{}/{}; max self-field ratio {:.4}",
            candidate.point_counts.estimate,
            candidate.point_counts.lower_bound,
            candidate.point_counts.unsupported,
            candidate.point_counts.along_current_excluded,
            candidate.max_self_field_ratio,
        );
    }
    println!(
        "Numerical {:?}; coverage {:?}; qualification {:?}; engineering {:?}",
        record.numerical_status,
        record.coverage_status,
        record.conductor_qualification_status,
        record.engineering_status
    );
    for check in &record.checks {
        println!("  {:?} {}: {}", check.status, check.id, check.detail);
    }
    println!(
        "Screening current allowance under declared assumptions ONLY; this is NOT a production operating-current limit."
    );
}

fn finish_coupled_search(
    record: &CoupledSearchRunRecord,
    args: &CoupledSearchArgs,
) -> Result<(), Box<dyn Error>> {
    if let Some(path) = &args.output {
        record.write_new(path)?;
        eprintln!("Saved {}", path.display());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(record)?);
    } else {
        print_coupled_search(record);
    }
    if record.search_status != Status::Pass || record.acceptance.agreement_status != Status::Pass {
        return Err(format!(
            "coupled search is not fully PASS (search_status {:?}, acceptance agreement {:?}); the report remains available for diagnosis",
            record.search_status, record.acceptance.agreement_status
        )
        .into());
    }
    Ok(())
}

fn print_coupled_search(record: &CoupledSearchRunRecord) {
    println!("OptCoil / {} / COUPLED COST SEARCH", record.case.id);
    println!(
        "{} candidates ({} PASS, {} FAIL); {:.1} ms total ({} threads, {} kernel evaluations, {} points evaluated, ~{:.0} kernel evaluations saved by pruning)",
        record.candidates.len(),
        record
            .candidates
            .iter()
            .filter(|c| c.status == Status::Pass)
            .count(),
        record
            .candidates
            .iter()
            .filter(|c| c.status == Status::Fail)
            .count(),
        record.elapsed_ms,
        record.runtime.execution_threads,
        record.kernel_evaluations,
        record.points_evaluated,
        record.estimated_kernel_evaluations_saved,
    );
    for c in &record.candidates {
        let screening_status = c.screening.as_ref().map(|s| s.status);
        let limiting = c
            .screening
            .as_ref()
            .and_then(|s| s.limiting.as_ref())
            .map(|l| {
                format!(
                    "{}/tape{}/turn{}/w{}",
                    l.station, l.tape_index, l.turn_index, l.width_index
                )
            })
            .unwrap_or_else(|| "none".into());
        let util = c
            .screening
            .as_ref()
            .and_then(|s| s.max_utilization)
            .map(|u| format!("{u:.4}"))
            .unwrap_or_else(|| "n/a".into());
        let pruned = match &c.pruned_by {
            Some(l) => format!(
                "{}/tape{}/turn{}/w{}",
                l.station, l.tape_index, l.turn_index, l.width_index
            ),
            None => "-".into(),
        };
        let strands = if c.geometry.strands_parallel > 1 {
            format!(" x{} strands", c.geometry.strands_parallel)
        } else {
            String::new()
        };
        let mech = match c.mechanical_feasible {
            Some(true) => " mechanical PASS".to_string(),
            Some(false) => " mechanical FAIL".to_string(),
            None => String::new(),
        };
        let hoop = c
            .hoop_stress_pa
            .map(|s| format!(" σ={:.1} MPa", s / 1e6))
            .unwrap_or_default();
        let transverse = c
            .transverse_pressure_pa
            .map(|p| format!(" p_t={:.2} MPa", p / 1e6))
            .unwrap_or_default();
        println!(
            "  {}x{}{strands} tapes (turns x pancakes, {} total turns): NI={:.3} A-turn I_op={:.4} A peak B={:.4} T | requirement {:?} refinement {:?} screening {:?}{mech}{hoop}{transverse} | limiting {limiting} util {util} | ${:.2} | pruned_by {pruned}",
            c.geometry.turns_along_normal,
            c.geometry.tapes_along_width,
            c.geometry.total_turns,
            c.ampere_turns_a,
            c.operating_current_a,
            c.peak_sampled_field_t.unwrap_or(f64::NAN),
            c.requirement_status,
            c.refinement_status,
            screening_status,
            c.cost.total_usd,
        );
    }
    let baseline = &record.candidates[record.baseline_index];
    println!(
        "Baseline {}x{}: ${:.2}; status {:?}",
        baseline.geometry.turns_along_normal,
        baseline.geometry.tapes_along_width,
        baseline.cost.total_usd,
        baseline.status
    );
    match record.best_index {
        Some(i) => {
            let best = &record.candidates[i];
            println!(
                "Best {}x{}: ${:.2}; status {:?}",
                best.geometry.turns_along_normal,
                best.geometry.tapes_along_width,
                best.cost.total_usd,
                best.status
            );
            if let (Some(opex), Some(lifecycle)) = (best.cost.opex_usd, best.cost.lifecycle_usd) {
                println!(
                    "Lifecycle: ${:.2} (capex ${:.2} + declared opex ${:.2})",
                    lifecycle, best.cost.total_usd, opex
                );
            }
            if record.candidates.len() == 1 {
                println!(
                    "Saving vs baseline: — (single candidate; it is both baseline and optimum)"
                );
            } else if record.best_index == Some(record.baseline_index) {
                println!(
                    "Saving vs baseline: 0% — baseline already optimal; no cheaper candidate passed"
                );
            } else {
                println!(
                    "Saving vs baseline: ${:.2} ({:.2}%) — modeled, synthetic prices, screening model",
                    record.savings_usd.unwrap_or(f64::NAN),
                    record.savings_percent.unwrap_or(f64::NAN)
                );
            }
        }
        None => println!("No PASS candidate exists; no saving to report."),
    }
    println!(
        "search_status {:?}; acceptance agreement {:?}",
        record.search_status, record.acceptance.agreement_status
    );
    for check in &record.acceptance.checks {
        println!(
            "  acceptance {:?} {}: {}",
            check.status, check.id, check.detail
        );
    }
    println!(
        "Screening-passing modeled-cost optimum under declared assumptions ONLY; this is NOT a production operating-current limit, and prices are invented placeholders — modeled, synthetic prices, screening model."
    );
}

fn finish_coupled_refine(
    record: &CoupledRefineRunRecord,
    args: &CoupledSearchArgs,
) -> Result<(), Box<dyn Error>> {
    if let Some(path) = &args.output {
        record.write_new(path)?;
        eprintln!("Saved {}", path.display());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(record)?);
    } else {
        print_coupled_refine(record);
    }
    if record.search_status != Status::Pass || record.acceptance.agreement_status != Status::Pass {
        return Err(format!(
            "coupled refine is not fully PASS (search_status {:?}, acceptance agreement {:?}); the report remains available for diagnosis",
            record.search_status, record.acceptance.agreement_status
        )
        .into());
    }
    Ok(())
}

fn print_coupled_refine(record: &CoupledRefineRunRecord) {
    println!("OptCoil / {} / COUPLED BRACKETING REFINE", record.case.id);
    println!(
        "{} pancake counts bisected; {:.1} ms total ({} threads, {} kernel evaluations, {} points evaluated)",
        record.pancakes.len(),
        record.elapsed_ms,
        record.runtime.execution_threads,
        record.kernel_evaluations,
        record.points_evaluated,
    );
    for p in &record.pancakes {
        let best = p
            .best_turns
            .map(|n| n.to_string())
            .unwrap_or_else(|| "none".into());
        let assignment = p
            .tape_spec_ids
            .as_ref()
            .map(|ids| format!(" [{}]", ids.join(",")))
            .unwrap_or_default();
        println!(
            "  {} tapes{assignment}: bracket [fail {}, pass {}] {:?}; {} candidates evaluated; n*={best}; monotonicity_ok {}; status {:?}",
            p.tapes_along_width,
            p.bracket.fail_turns,
            p.bracket.pass_turns,
            p.bracket_status,
            p.evaluated.len(),
            p.monotonicity_ok,
            p.status,
        );
    }
    println!(
        "Baseline {}x{}: ${:.2}; status {:?}",
        record.case.baseline.turns_along_normal,
        record.case.baseline.tapes_along_width,
        record.baseline.cost.total_usd,
        record.baseline.status
    );
    match &record.global_optimum {
        Some(best) => {
            let assignment = best
                .tape_spec_ids
                .as_ref()
                .map(|ids| format!(" [{}]", ids.join(",")))
                .unwrap_or_default();
            println!(
                "Global optimum {}x{}{}: ${:.2}; status {:?}; utilization margin to limit {:?}",
                best.turns_along_normal,
                best.tapes_along_width,
                assignment,
                best.candidate.cost.total_usd,
                best.candidate.status,
                record.optimum_utilization_margin,
            );
            println!(
                "Saving vs the declared baseline: ${:.2} ({:.2}%) — modeled, synthetic prices, screening model",
                record.savings_usd.unwrap_or(f64::NAN),
                record.savings_percent.unwrap_or(f64::NAN)
            );
        }
        None => println!("No pancake count has a valid, monotone bracket; no global optimum."),
    }
    println!(
        "search_status {:?}; acceptance agreement {:?}",
        record.search_status, record.acceptance.agreement_status
    );
    for check in &record.acceptance.checks {
        println!(
            "  acceptance {:?} {}: {}",
            check.status, check.id, check.detail
        );
    }
    println!(
        "Screening-passing modeled-cost optimum under declared assumptions ONLY; this is NOT a production operating-current limit, and prices are invented placeholders — modeled, synthetic prices, screening model. The design sits at the declared limits by construction."
    );
}

fn geometry_label_cli(g: &optcoil_search::coupled_search::CandidateGeometry) -> String {
    let base = format!("{}x{}", g.turns_along_normal, g.tapes_along_width);
    if g.strands_parallel > 1 {
        format!("{base}x{}s", g.strands_parallel)
    } else {
        base
    }
}

fn record_optimum_total(record_json: &str) -> f64 {
    serde_json::from_str::<serde_json::Value>(record_json)
        .ok()
        .and_then(|v| {
            let i = v["best_index"].as_u64()? as usize;
            v["candidates"][i]["cost"]["total_usd"].as_f64()
        })
        .unwrap_or(f64::NAN)
}

fn unix_now_ms() -> Result<u64, Box<dyn Error>> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64)
}

/// Writes a file, refusing to overwrite — run artifacts and key material
/// are never silently clobbered.
fn write_review_package(
    directory: &Path,
    artifacts: &[(String, String)],
) -> Result<(), Box<dyn Error>> {
    // A new directory prevents merging partial or stale files with a previous review.
    fs::create_dir(directory)?;
    for (path, contents) in artifacts {
        let destination = directory.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        write_new(&destination, contents)?;
    }
    Ok(())
}

fn write_new(path: &Path, text: &str) -> Result<(), Box<dyn Error>> {
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    use std::io::Write as _;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn execute_dataset(command: DatasetCommand) -> Result<(), Box<dyn Error>> {
    match command {
        DatasetCommand::Build {
            metadata,
            csv,
            output,
        } => {
            let metadata_json = fs::read_to_string(&metadata)?;
            let csv_text = fs::read_to_string(&csv)
                .map_err(|e| format!("material CSV must be UTF-8 text: {e}"))?;
            let dataset = MaterialDataset::from_csv(&metadata_json, csv_text.as_bytes())?;
            let bundle = MaterialBundle::from_parts(dataset, csv_text, None)?;
            let text = bundle.to_json()?;
            // Round-trip: the written bundle must re-validate.
            MaterialBundle::from_json(&text)?;
            write_new(&output, &text)?;
            eprintln!(
                "Saved unsigned dataset bundle {} ({} / {} measurements, csv sha {})",
                output.display(),
                bundle.dataset.metadata.id,
                bundle.dataset.points.len(),
                &bundle.dataset.metadata.csv_sha256[..16],
            );
        }
        DatasetCommand::Keygen {
            issuer,
            key_id,
            output,
        } => {
            let mut secret = [0u8; 32];
            rand_core::OsRng.fill_bytes(&mut secret);
            let key = SigningKeyFile::new(&issuer, &key_id, secret)?;
            let text = serde_json::to_string_pretty(&key)?;
            write_new(&output, &text)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&output, fs::Permissions::from_mode(0o600))?;
            }
            println!("issuer:     {}", key.issuer);
            println!("key_id:     {}", key.key_id);
            println!("public key: {}", key.public_key);
            eprintln!(
                "Secret key written to {} — keep it private; only the public key goes into a registry.",
                output.display()
            );
        }
        DatasetCommand::Attest {
            bundle,
            key,
            output,
        } => {
            let mut parsed = MaterialBundle::from_json(&fs::read_to_string(&bundle)?)?;
            let key_file = SigningKeyFile::from_json(&fs::read_to_string(&key)?)?;
            let attestation = DatasetAttestation::create(
                &key_file.issuer,
                &key_file.key_id,
                &parsed.dataset.metadata.id,
                &parsed.dataset.metadata.csv_sha256,
                unix_now_ms()?,
                &key_file.signing_key()?,
            )?;
            parsed.attestation = Some(attestation);
            let text = parsed.to_json()?;
            MaterialBundle::from_json(&text)?;
            write_new(&output, &text)?;
            let att = parsed.attestation.as_ref().unwrap();
            eprintln!(
                "Saved attested bundle {} — {} (key {}) signed {} csv_sha={}…",
                output.display(),
                att.issuer,
                att.key_id,
                parsed.dataset.metadata.id,
                &parsed.dataset.metadata.csv_sha256[..16],
            );
        }
        DatasetCommand::Verify {
            bundle,
            registry,
            pubkey,
            json,
        } => {
            let bundle_text = fs::read_to_string(&bundle)?;
            let registry_text = registry.map(|p| fs::read_to_string(&p)).transpose()?;
            let checks = optcoil_search::verify::verify_dataset_bundle(
                &bundle_text,
                registry_text.as_deref(),
                pubkey.as_deref(),
            )
            .map_err(|e| e.to_string())?;
            let n_fail = checks.iter().filter(|c| c.verdict == "FAIL").count();
            if json {
                let lines: Vec<serde_json::Value> = checks
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "check": c.name,
                            "verdict": c.verdict,
                            "detail": c.detail,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&lines)?);
            } else {
                for c in &checks {
                    println!("{:12} {}: {}", c.verdict, c.name, c.detail);
                }
                let n_pass = checks.iter().filter(|c| c.verdict == "PASS").count();
                let n_nc = checks.iter().filter(|c| c.verdict == "NOT_CHECKED").count();
                println!("\n{n_pass} PASS, {n_fail} FAIL, {n_nc} NOT_CHECKED");
                println!(
                    "Verified: bundle structure, csv binding, attestation signature and \
                     registry status. Not verified: the measurements themselves — a \
                     countersignature attests provenance and integrity, not physics."
                );
            }
            if n_fail > 0 {
                return Err(format!("dataset verify: {n_fail} check(s) failed").into());
            }
        }
        DatasetCommand::RegistryUpsert {
            bundle,
            registry,
            key,
            status,
            note,
            add_key,
        } => {
            let key_file = SigningKeyFile::from_json(&fs::read_to_string(&key)?)?;
            let registry_key = key_file.signing_key()?;
            let bundle_text = fs::read_to_string(&bundle)?;
            let parsed = MaterialBundle::from_json(&bundle_text)?;
            let attestation = parsed.attestation.as_ref().ok_or(
                "registry entries require an attested bundle — run `dataset attest` first",
            )?;
            let mut reg = if registry.exists() {
                let r = DatasetRegistry::from_json(&fs::read_to_string(&registry)?)?;
                if r.registry_issuer != key_file.issuer {
                    return Err(format!(
                        "registry issuer is {} but key file is for {}",
                        r.registry_issuer, key_file.issuer
                    )
                    .into());
                }
                r
            } else {
                DatasetRegistry::empty(&key_file.issuer, &key_file, unix_now_ms()?)
            };
            if let Some(key_path) = add_key {
                let issuer_key = SigningKeyFile::from_json(&fs::read_to_string(&key_path)?)?;
                reg.keys
                    .retain(|k| !(k.issuer == issuer_key.issuer && k.key_id == issuer_key.key_id));
                reg.keys.push(RegistryKey {
                    issuer: issuer_key.issuer,
                    key_id: issuer_key.key_id,
                    public_key: issuer_key.public_key,
                    status: "current".into(),
                });
                reg.keys
                    .sort_by(|a, b| a.issuer.cmp(&b.issuer).then(a.key_id.cmp(&b.key_id)));
            }
            let bundle_sha = sha256_hex(bundle_text.as_bytes());
            let att_sha = attestation_sha256(attestation);
            let entry = RegistryEntry::countersigned(
                &reg.registry_issuer,
                &parsed.dataset.metadata.id,
                &bundle_sha,
                &att_sha,
                &status,
                &note,
                &registry_key,
            )?;
            reg.entries.retain(|e| e.dataset_id != entry.dataset_id);
            reg.entries.push(entry);
            reg.entries.sort_by(|a, b| a.dataset_id.cmp(&b.dataset_id));
            reg.issued_at_unix_ms = unix_now_ms()?;
            fs::write(&registry, serde_json::to_string_pretty(&reg)?)?;
            eprintln!(
                "Registry {} updated: {} → {} (bundle sha {}…)",
                registry.display(),
                parsed.dataset.metadata.id,
                status,
                &bundle_sha[..16],
            );
        }
    }
    Ok(())
}
