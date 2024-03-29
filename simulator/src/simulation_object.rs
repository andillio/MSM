use crate::{
    ics::*,
    utils::{
        complex::complex_constant,
        error::*,
        fft::{forward, forward_inplace, inverse, inverse_inplace, spec_grid},
        grid::{check_complex_for_nans, check_norm, Dimensions, IntoT},
    },
};
use anyhow::Result;
use arrayfire::{
    abs, conjg, div, exp, isnan, max_all, mul, real, replace_scalar, Array, ComplexFloating,
    ConstGenerator, Dim4, FloatingPoint, Fromf64, HasAfEnum,
};
#[cfg(feature = "expanding")]
use {
    crate::{expanding::ScaleFactorSolver, utils::rk4},
    msm_common::{constants::*, get_supercomoving_boxsize, CosmologyParameters},
};

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use msm_common::{
    constants::{HBAR, POIS_CONST},
    InitialConditions, SamplingParameters,
};
use ndarray_npy::{ReadableElement, WritableElement};
use num::{Complex, Float, FromPrimitive, ToPrimitive};
use num_traits::FloatConst;
use serde::Serialize;
use std::fmt::{Display, LowerExp};
use std::time::Instant;

#[cfg(not(feature = "remote-storage"))]
use crate::utils::io::complex_array_to_disk;
#[cfg(feature = "remote-storage")]
use crate::utils::io::RemoteStorage;

// Maximum number of concurrent writes to disk/remote
const MAX_CONCURRENT_GRID_WRITES: usize = 16;

/// This struct holds the grids which store the wavefunction, its Fourier transform, and other grids
pub struct SimulationGrid<T>
where
    T: Float
        + FloatingPoint
        + ConstGenerator<OutType = T>
        + HasAfEnum<InType = T>
        + HasAfEnum<BaseType = T>
        + FromPrimitive,
    Complex<T>: HasAfEnum
        + FloatingPoint
        + HasAfEnum<AbsOutType = T>
        + ConstGenerator<OutType = Complex<T>>,
{
    // Spatial Fields
    /// The array which stores the wavefunction
    pub ψ: Array<Complex<T>>,

    /// Fourier space
    pub ψk: Array<Complex<T>>,

    /// Potential
    pub φ: Array<Complex<T>>,
}

/// This `Parameters` struct stores simulations parameters
pub struct SimulationParameters<T: Float + FloatingPoint> {
    // Grid Parameters
    /// Physical length of each axis (if expanding, this is the initial length)
    pub axis_length: T,
    /// Comoving length of each axis
    #[cfg(feature = "expanding")]
    pub comoving_boxsize: T,
    /// Spatial cell size
    pub dx: T,
    /// k-space cell size
    pub dk: T,
    /// Fourier grid (k^2)
    pub spec_grid: Array<T>,
    /// Max of Fourier grid
    pub k2_max: T,
    /// Dimensionality of grid
    pub dims: Dimensions,
    /// Number of grid cells
    pub size: usize,

    // Temporal Parameters
    /// Current simulation time
    pub time: T,
    /// Current simulation time (comoving)
    #[cfg(feature = "expanding")]
    pub tau: T,
    /// Total simulation time
    pub final_sim_time: T,
    /// Total simulation time (comoving)
    #[cfg(feature = "expanding")]
    pub final_sim_tau: T,
    /// Total number of data dumps
    pub num_data_dumps: u32,
    /// Current number of data dumps
    pub current_dumps: u32,
    /// Current number of time steps
    pub time_steps: u64,
    /// Timestep Criterion
    pub cfl: T,

    // Physical Parameters
    /// Total Mass
    pub total_mass: f64,
    /// Particle mass
    pub particle_mass: f64,
    /// HBAR tilde (HBAR / particle_mass)
    pub hbar_: T,
    /// Total number of particles
    pub n_tot: f64,

    // Simulation parameters and metadata
    /// Simulation name
    pub sim_name: String,
    /// Fourier alias bound, in [0, 1]
    pub k2_cutoff: f64,
    /// Alias threshold (probability mass), in [0,1]
    pub alias_threshold: f64,
    /// Simulation wall time (millis)
    pub sim_wall_time: u128,
    /// Number of timesteps taken
    pub n_steps: u64,
    /// Quantum distribution sampling parameters
    pub sampling_parameters: Option<SamplingParameters>,
    /// Initial conditions
    pub ics: InitialConditions,
    /// Flag indicating whether we output potential
    pub output_potential: bool,

    #[cfg(feature = "expanding")]
    pub cosmo_params: CosmologyParameters,

    #[cfg(feature = "remote-storage")]
    pub remote_storage: RemoteStorage,
}

/// In the original python implementation, this was a `sim` or `SimObject` object.
/// This stores a `SimulationGrid` which has the wavefunction and its fourier transform.
/// It also holds the `SimulationParameters` which holds the simulation parameters.
pub struct SimulationObject<T>
where
    T: Float
        + FloatingPoint
        + ConstGenerator<OutType = T>
        + HasAfEnum<InType = T>
        + HasAfEnum<BaseType = T>
        + FromPrimitive
        + std::fmt::LowerExp
        + FloatConst
        + Send
        + Sync
        + 'static,
    Complex<T>: HasAfEnum
        + ComplexFloating
        + FloatingPoint
        + HasAfEnum<AbsOutType = T>
        + HasAfEnum<ArgOutType = T>
        + ConstGenerator<OutType = Complex<T>>,
{
    /// This has the wavefunction and its Fourier transform
    pub grid: SimulationGrid<T>,

    /// This has the simulation parameters
    pub parameters: SimulationParameters<T>,

    /// Active io (local storage), return value is time elapsed for io
    #[cfg(not(feature = "remote-storage"))]
    pub active_io: Vec<std::thread::JoinHandle<u128>>,

    /// Active io (remote storage), return value is url where grid is hosted
    #[cfg(feature = "remote-storage")]
    pub active_io: Vec<tokio::task::JoinHandle<String>>,

    /// Progress bar
    pub pb: ProgressBar,

    #[cfg(feature = "expanding")]
    scale_factor_solver: ScaleFactorSolver,
}

impl<T> SimulationGrid<T>
where
    T: Float
        + FloatingPoint
        + ConstGenerator<OutType = T>
        + HasAfEnum<InType = T>
        + HasAfEnum<BaseType = T>
        + FromPrimitive,
    Complex<T>: HasAfEnum
        + ComplexFloating
        + FloatingPoint
        + HasAfEnum<ComplexOutType = Complex<T>>
        + HasAfEnum<UnaryOutType = Complex<T>>
        + HasAfEnum<AbsOutType = T>
        + HasAfEnum<ArgOutType = T>
        + ConstGenerator<OutType = Complex<T>>,
{
    pub fn new(ψ: Array<Complex<T>>) -> Self {
        SimulationGrid {
            φ: real(&ψ).cast(), // Note: Initialized with incorrect values!
            ψk: ψ.clone(),      // Note: Initialized with incorrect values!
            ψ,
        }
    }
}

impl<T> SimulationParameters<T>
where
    T: FromPrimitive
        + Float
        + FloatingPoint
        + Display
        + HasAfEnum<InType = T>
        + HasAfEnum<BaseType = T>
        + Fromf64
        + ConstGenerator<OutType = T>,
{
    pub fn new(
        axis_length: T,
        time: T,
        final_sim_time: T,
        cfl: T,
        num_data_dumps: u32,
        total_mass: f64,
        particle_mass: f64,
        sim_name: String,
        k2_cutoff: f64,
        alias_threshold: f64,
        hbar_: Option<f64>,
        dims: Dimensions,
        size: usize,
        output_potential: bool,
        #[cfg(feature = "expanding")] cosmo_params: CosmologyParameters,
        sampling_parameters: Option<SamplingParameters>,
        ics: InitialConditions,
        #[cfg(feature = "remote-storage")] remote_storage: RemoteStorage,
    ) -> Self {
        let hbar_: T = T::from_f64(hbar_.unwrap_or(HBAR / particle_mass)).unwrap();

        #[cfg(feature = "expanding")]
        let tau = T::from(get_tau(time.to_f64().unwrap(), cosmo_params)).unwrap();
        #[cfg(feature = "expanding")]
        let final_sim_tau =
            T::from(get_tau(final_sim_time.to_f64().unwrap(), cosmo_params)).unwrap();
        #[cfg(feature = "expanding")]
        let comoving_boxsize = T::from(get_supercomoving_boxsize(
            hbar_.to_f64().unwrap(),
            cosmo_params,
            axis_length.to_f64().unwrap(),
        ))
        .unwrap();

        // Overconstrained parameters
        #[cfg(not(feature = "expanding"))]
        let dx = axis_length / T::from_usize(size).unwrap();
        #[cfg(feature = "expanding")]
        let dx = comoving_boxsize / T::from_usize(size).unwrap();
        let dk = dx;
        let n_tot = total_mass / particle_mass;

        // Counter variables
        let current_dumps = 0;
        let time_steps = 0;
        let n_steps = 0;
        let sim_wall_time = 0;

        // Spectral grid
        let spec_grid = spec_grid::<T>(dx, dims, size);
        let k2_max: T = max_all(&spec_grid).0;

        log::debug!("spec_grid k2 max is {k2_max}");

        SimulationParameters {
            axis_length,
            dx,
            dk,
            time,
            final_sim_time,
            cfl,
            num_data_dumps,
            current_dumps,
            time_steps,
            total_mass,
            particle_mass,
            hbar_,
            sim_name,
            k2_cutoff,
            alias_threshold,
            spec_grid,
            k2_max,
            sim_wall_time,
            n_tot,
            size,
            dims,
            n_steps,
            sampling_parameters,
            output_potential,
            ics,
            #[cfg(feature = "expanding")]
            comoving_boxsize,
            #[cfg(feature = "expanding")]
            cosmo_params,
            #[cfg(feature = "expanding")]
            tau,
            #[cfg(feature = "expanding")]
            final_sim_tau,
            #[cfg(feature = "remote-storage")]
            remote_storage,
        }
    }

    pub fn get_shape(&self) -> [u64; 4] {
        match self.dims {
            Dimensions::One => [self.size as u64, 1, 1, 1],
            Dimensions::Two => [self.size as u64, self.size as u64, 1, 1],
            Dimensions::Three => [self.size as u64, self.size as u64, self.size as u64, 1],
        }
    }
}
impl<U> Display for SimulationParameters<U>
where
    U: Float + FloatingPoint + Display + LowerExp,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n", "-".repeat(40))?;
        write!(f, "axis_length         = {:.6e}\n", self.axis_length)?;
        #[cfg(feature = "expanding")]
        write!(f, "comoving_boxsize    = {:.6e}\n", self.comoving_boxsize)?;
        write!(f, "dx                  = {:.6e}\n", self.dx)?;
        write!(f, "current_time        = {:.6e}\n", self.time)?;
        #[cfg(feature = "expanding")]
        write!(f, "current_tau       = {:.6e}\n", self.tau)?;
        write!(f, "final_sim_time      = {:.6e}\n", self.final_sim_time)?;
        #[cfg(feature = "expanding")]
        write!(f, "final_sim_tau       = {:.6e}\n", self.final_sim_tau)?;
        write!(f, "cfl                 = {:.6e}\n", self.cfl)?;
        write!(f, "num_data_dumps      = {:.6e}\n", self.num_data_dumps)?;
        write!(f, "total_mass          = {:.6e}\n", self.total_mass)?;
        write!(f, "particle_mass       = {:.6e}\n", self.particle_mass)?;
        write!(f, "hbar_               = {:.6e}\n", self.hbar_)?;
        write!(f, "sim_name            = {}\n", self.sim_name)?;
        write!(f, "k2_cutoff           = {:.6e}\n", self.k2_cutoff)?;
        write!(f, "alias_threshold     = {:.6e}\n", self.alias_threshold)?;
        write!(f, "k2_max              = {:.6e}\n", self.k2_max)?;
        write!(f, "n_tot               = {:.6e}\n", self.n_tot)?;
        write!(f, "dims                = {}\n", self.dims as usize)?;
        write!(f, "size                = {}\n", self.size as usize)?;
        write!(f, "{}\n", "-".repeat(40))?;
        #[cfg(feature = "expanding")]
        write!(f, "\n{:#?}\n", self.cosmo_params)?;
        if let Some(ref sampling) = self.sampling_parameters {
            write!(f, "\n[sampling_parameters]\n")?;
            write!(f, "{:20}= {:?}\n", "sampling_scheme", sampling.scheme)?;
            write!(f, "{:20}= {:?}\n", "seed", sampling.seed)?;
        }
        Ok(())
    }
}

impl<T> SimulationObject<T>
where
    T: Float
        + FloatingPoint
        + Display
        + ToPrimitive
        + FromPrimitive
        + ConstGenerator<OutType = T>
        + HasAfEnum<InType = T>
        + HasAfEnum<AbsOutType = T>
        + HasAfEnum<AggregateOutType = T>
        + HasAfEnum<BaseType = T>
        + Fromf64
        + WritableElement
        + ReadableElement
        + std::fmt::LowerExp
        + FloatConst
        + Send
        + Sync
        + Default
        + Serialize
        + std::fmt::Debug
        + 'static,
    Complex<T>: HasAfEnum
        + FloatingPoint
        + ComplexFloating
        + HasAfEnum<AggregateOutType = Complex<T>>
        + HasAfEnum<BaseType = T>
        + HasAfEnum<ComplexOutType = Complex<T>>
        + HasAfEnum<UnaryOutType = Complex<T>>
        + HasAfEnum<AbsOutType = T>
        + HasAfEnum<ArgOutType = T>
        + ConstGenerator<OutType = Complex<T>>
        + Default
        + Serialize
        + std::fmt::Debug,
    rand_distr::Standard: rand_distr::Distribution<T>,
{
    /// A constructor function which returns a `SimulationObject` from a user's toml.
    pub fn new_from_params(parameters: SimulationParameters<T>) -> Result<Self, RuntimeError> {
        // Construct wavefunction
        let mut grid: SimulationGrid<T> = match &parameters.ics {
            // User-specified Initial Conditions
            InitialConditions::UserSpecified { path } => {
                SimulationGrid::<T>::new(user_specified_ics(path, &parameters))
            }

            // Real space gaussian
            InitialConditions::ColdGauss { mean, std } => {
                cold_gauss::<T>(mean.into_t(), std.into_t(), &parameters)
            }

            // Momentum space gaussian with random phases
            InitialConditions::ColdGaussKSpace {
                mean,
                std,
                phase_seed,
            } => cold_gauss_kspace::<T>(mean.into_t(), std.into_t(), &parameters, *phase_seed),

            // Spherical Tophat
            &InitialConditions::SphericalTophat {
                radius,
                delta,
                slope,
            } => spherical_tophat::<T>(&parameters, radius, delta, slope),
        };

        // Apply quantum perturbations if specified
        if let Some(ref sampling_parameters) = &parameters.sampling_parameters {
            sample_quantum_perturbation(&mut grid, &parameters, sampling_parameters);
        }

        #[cfg(feature = "expanding")]
        let scale_factor_solver = ScaleFactorSolver::new(parameters.cosmo_params);

        let pb = ProgressBar::with_draw_target(
            parameters.num_data_dumps as u64,
            ProgressDrawTarget::stdout(),
        );

        pb.set_style(ProgressStyle::default_bar().template(
            "[{elapsed_precise}; {eta_precise}] {bar:20.cyan/blue} {pos:>5}/{len:5} {msg}",
        ));

        // Pack grid and parameters into `Simulation Object`
        let sim_obj = SimulationObject {
            grid,
            parameters,
            active_io: vec![],
            #[cfg(feature = "expanding")]
            scale_factor_solver,
            pb,
        };

        debug_assert!(check_norm::<T>(
            &sim_obj.grid.ψ,
            sim_obj.parameters.dx,
            sim_obj.parameters.dims
        ));
        debug_assert!(check_norm::<T>(
            &sim_obj.grid.ψk,
            sim_obj.parameters.dk,
            sim_obj.parameters.dims
        ));

        Ok(sim_obj)
    }

    /// This function updates the `SimulationGrid` stored in the `SimulationObject`.
    #[cfg(not(feature = "expanding"))]
    pub fn update(&mut self, _verbose: bool) -> Result<()> {
        // If this is the first timestep, populate the kspace grid with the correct values
        if self.parameters.n_steps == 0 {
            self.grid.ψk = self.forward(&self.grid.ψ).unwrap();
        };

        // Begin timer for update loop
        let now = Instant::now();

        // Initial checks
        debug_assert!(check_norm::<T>(
            &self.grid.ψ,
            self.parameters.dx,
            self.parameters.dims
        ));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));

        // Calculate potential at t
        self.calculate_potential();
        debug_assert!(check_complex_for_nans(&self.grid.φ));
        // Compute timestep
        let (dump, dt) = self.get_timestep();

        // Update kinetic half-step
        // exp(-(dt/2) * (k^2 / 2) * h_) = exp(-dt/4 * h_ * k^2)
        let k_evolution: Array<Complex<T>> = exp(&mul(
            &complex_constant(
                Complex::<T>::new(
                    T::zero(),
                    -dt / T::from_f64(4.0).unwrap() * self.parameters.hbar_,
                ),
                (1, 1, 1, 1),
            ),
            &self.parameters.spec_grid.cast(),
            true,
        ));
        // These are the fields with kinetic at t + dt/2 but momentum at t
        self.grid.ψk = mul(&self.grid.ψk, &k_evolution, false);
        debug_assert!(check_complex_for_nans(&self.grid.ψk));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));
        self.grid.ψ = self.inverse(&self.grid.ψk).unwrap();
        debug_assert!(check_complex_for_nans(&self.grid.ψ));
        debug_assert!(check_norm::<T>(
            &self.grid.ψ,
            self.parameters.dx,
            self.parameters.dims
        ));
        self.calculate_potential();
        debug_assert!(check_complex_for_nans(&self.grid.φ));

        // Update momentum a full-step
        // exp(-dt * φ / h_) = exp(-(dt/h_) * φ)
        let r_evolution: Array<Complex<T>> = exp(&mul(
            &complex_constant(
                Complex::<T>::new(T::zero(), -dt / self.parameters.hbar_),
                (1, 1, 1, 1),
            ),
            &self.grid.φ.cast(),
            true,
        ));
        //complex_array_to_disk("r_evo", "r_evo", &r_evolution, [shape.0, shape.1, shape.2, shape.3]);
        // these are the fields with kinetic at t + dt/2 but momentum at t + dt
        self.grid.ψ = mul(&self.grid.ψ, &r_evolution, false);
        debug_assert!(check_complex_for_nans(&self.grid.ψ));
        debug_assert!(check_norm::<T>(
            &self.grid.ψ,
            self.parameters.dx,
            self.parameters.dims
        ));
        self.grid.ψk = self.forward(&self.grid.ψ).unwrap();
        debug_assert!(check_complex_for_nans(&self.grid.ψk));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));

        // Update kinetic from t + dt/2 to t + dt
        // exp(-(dt/2) * (k^2/2) * h_) = exp(-dt/4 * h_ * k^2)
        let k_evolution: Array<Complex<T>> = exp(&mul(
            &complex_constant(
                Complex::<T>::new(
                    T::zero(),
                    -dt / T::from_f64(4.0).unwrap() * self.parameters.hbar_,
                ),
                (1, 1, 1, 1),
            ),
            &self.parameters.spec_grid.cast(),
            true,
        ));
        // Now all fields have kinetic + momentum at t + dt
        self.grid.ψk = mul(&self.grid.ψk, &k_evolution, false);
        debug_assert!(check_complex_for_nans(&self.grid.ψk));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));
        self.grid.ψ = self.inverse(&self.grid.ψk)?;
        debug_assert!(check_complex_for_nans(&self.grid.ψ));
        debug_assert!(check_norm::<T>(
            &self.grid.ψ,
            self.parameters.dx,
            self.parameters.dims
        ));

        // Update time
        self.parameters.time = self.parameters.time + dt;

        // Print estimate of time to completion
        // if verbose {
        //     let estimate = now.elapsed().as_millis()
        //         * T::to_u128(&((self.parameters.final_sim_time - self.parameters.time) / dt))
        //             .unwrap_or(1);
        //     println!(
        //         "update took {} millis, current sim time is {:e}, dt is {:e}. ETA {:?} ",
        //         now.elapsed().as_millis(),
        //         self.parameters.time,
        //         dt,
        //         std::time::Duration::from_millis(estimate as u64)
        //     );
        // }

        // Check for Fourier Aliasing
        if let Some(alias_mass) = self.check_alias() {
            // If above threshold, bail and report aliasing
            panic!(
                "simulation aliased {}",
                RuntimeError::FourierAliasing {
                    threshold: self.parameters.alias_threshold,
                    k2_cutoff: self.parameters.k2_cutoff,
                    p_mass: T::to_f64(&alias_mass).unwrap(),
                }
            );
        }

        // Perform data dump if appropriate
        if dump {
            // Increment before dump for proper dump name
            self.parameters.current_dumps = self.parameters.current_dumps + 1;

            // Dump wavefunction
            self.dump();

            // TODO: fix for initial_time != 0
            self.parameters.time = T::from_u32(self.parameters.current_dumps).unwrap()
                * self.parameters.final_sim_time
                / T::from_u32(self.parameters.num_data_dumps).unwrap();
        }

        // Increment wall time counter, step counter
        self.parameters.sim_wall_time += now.elapsed().as_millis();
        self.parameters.n_steps += 1;

        // If finished, wait for I/O to finish
        if !self.not_finished() {
            // If using remote storage, get url pairs and print them
            #[cfg(feature = "remote-storage")]
            while !self.active_io.is_empty() {
                let _grid = self
                    .parameters
                    .remote_storage
                    .rt
                    .block_on(self.active_io.remove(0))
                    .expect("remote io failed");
                // println!("uploaded {grid}");
            }

            // If using local storage, wait for io to finish
            #[cfg(not(feature = "remote-storage"))]
            while !self.active_io.is_empty() {
                self.active_io.remove(0).join().unwrap();
            }

            self.pb.finish();
        }

        Ok(())
    }

    /// This function updates the `SimulationGrid` stored in the `SimulationObject`.
    /// For `feature = "expanding"`, this varies in two ways:
    /// 1) dt --> dtau, which removes factors of hbar_
    /// 2) Doing the half-step is done twice explicitly for the momentum update, since
    ///    a(t) needs to be updated
    #[cfg(feature = "expanding")]
    pub fn update(&mut self, _verbose: bool) -> Result<()> {
        // If this is the first timestep, populate the kspace grid with the correct values

        if self.parameters.n_steps == 0 {
            self.grid.ψk = self.forward(&self.grid.ψ).unwrap();
        };

        // Begin timer for update loop
        let now = Instant::now();

        // Initial checks
        debug_assert!(check_norm::<T>(
            &self.grid.ψ,
            self.parameters.dx,
            self.parameters.dims
        ));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));

        // Calculate potential at t
        self.calculate_potential();
        debug_assert!(check_complex_for_nans(&self.grid.φ));
        // Compute timestep
        let (dump, dtau) = self.get_timestep();

        // Update kinetic half-step
        // exp(-(dt/2) * (k^2 / 2)) = exp(-dt/4 * k^2)
        let k_evolution: Array<Complex<T>> = exp(&mul(
            &complex_constant(
                Complex::<T>::new(T::zero(), -dtau / T::from_f64(4.0).unwrap()),
                (1, 1, 1, 1),
            ),
            &self.parameters.spec_grid.cast(),
            true,
        ));
        // These are the fields with kinetic at t + dt/2 but momentum at t
        self.grid.ψk = mul(&self.grid.ψk, &k_evolution, false);
        debug_assert!(check_complex_for_nans(&self.grid.ψk));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));
        self.grid.ψ = self.inverse(&self.grid.ψk).unwrap();
        debug_assert!(check_complex_for_nans(&self.grid.ψ));
        debug_assert!(check_norm::<T>(
            &self.grid.ψ,
            self.parameters.dx,
            self.parameters.dims
        ));
        self.calculate_potential();
        debug_assert!(check_complex_for_nans(&self.grid.φ));

        // Update momentum a half-step twice (evolving a in between)
        for _ in 0..2 {
            // exp(- (dt/2 * a(t)) φ )
            let a = self.get_scale_factor_T();
            let r_evolution: Array<Complex<T>> = exp(&mul(
                &complex_constant(
                    Complex::<T>::new(
                        T::zero(),
                        -dtau / T::from_f64(2.0).unwrap() * a, // / self.parameters.hbar_.sqrt(/* TODO: debug */),
                    ),
                    (1, 1, 1, 1),
                ),
                &self.grid.φ.cast(),
                true,
            ));

            // do the half-step
            self.grid.ψ = mul(&self.grid.ψ, &r_evolution, false);
            debug_assert!(check_complex_for_nans(&self.grid.ψ));
            debug_assert!(check_norm::<T>(
                &self.grid.ψ,
                self.parameters.dx,
                self.parameters.dims
            ));

            // Find what dtau / 2 is in megayears
            let dtau_over_2_in_megayears =
                self.calculate_dt_from_dtau(dtau / T::from(2.0).unwrap());

            // The solver (external library) expects megayears as f64
            self.scale_factor_solver
                .step(dtau_over_2_in_megayears.to_f64().unwrap());
            self.parameters.time = self.parameters.time + dtau_over_2_in_megayears;
            // Update time (comoving)
            self.parameters.tau = self.parameters.tau + dtau / T::from_f64(2.0).unwrap();
        }
        self.grid.ψk = self.forward(&self.grid.ψ).unwrap();
        debug_assert!(check_complex_for_nans(&self.grid.ψk));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));

        // Update kinetic from t + dt/2 to t + dt
        // exp(-(dt/2) * (k^2/2) / h) = exp(-dt/4 * k^2)
        let k_evolution: Array<Complex<T>> = exp(&mul(
            &complex_constant(
                Complex::<T>::new(T::zero(), -dtau / T::from_f64(4.0).unwrap()),
                (1, 1, 1, 1),
            ),
            &self.parameters.spec_grid.cast(),
            true,
        ));
        // Now all fields have kinetic + momentum at t + dt
        self.grid.ψk = mul(&self.grid.ψk, &k_evolution, false);
        debug_assert!(check_complex_for_nans(&self.grid.ψk));
        debug_assert!(check_norm::<T>(
            &self.grid.ψk,
            self.parameters.dk,
            self.parameters.dims
        ));
        self.grid.ψ = self.inverse(&self.grid.ψk)?;
        debug_assert!(check_complex_for_nans(&self.grid.ψ));
        debug_assert!(check_norm::<T>(
            &self.grid.ψ,
            self.parameters.dx,
            self.parameters.dims
        ));

        // Increment wall time counter, step counter
        self.parameters.sim_wall_time += now.elapsed().as_millis();
        self.parameters.n_steps += 1;

        // if verbose {
        //     // Print estimate of time to completion
        //     let estimate = now.elapsed().as_millis()
        //         * T::to_u128(&((self.parameters.final_sim_tau - self.parameters.tau) / dtau))
        //             .unwrap_or(1);
        //     println!(
        //         "update took {} millis, current sim time is {:.5e} (z = {:.3}), dtau is {:.3e}. ETA {:?} ",
        //         now.elapsed().as_millis(),
        //         self.parameters.time,
        //         self.get_scale_factor().recip() - 1.0,
        //         dtau,
        //         std::time::Duration::from_millis(estimate as u64)
        //     );
        // }

        // Check for Fourier Aliasing
        if let Some(alias_mass) = self.check_alias() {
            // If above threshold, bail and report aliasing
            panic!(
                "simulation aliased {:?}",
                RuntimeError::FourierAliasing {
                    threshold: self.parameters.alias_threshold,
                    k2_cutoff: self.parameters.k2_cutoff,
                    p_mass: T::to_f64(&alias_mass).unwrap()
                }
            );
        }

        // Perform data dump if appropriate
        if dump {
            // Increment before dump for proper dump name
            self.parameters.current_dumps = self.parameters.current_dumps + 1;

            // Dump wavefunction
            self.dump();

            // TODO: fix for initial_time != 0
            self.parameters.time = T::from_u32(self.parameters.current_dumps).unwrap()
                * self.parameters.final_sim_time
                / T::from_u32(self.parameters.num_data_dumps).unwrap();
            self.parameters.tau = T::from(get_tau(
                self.parameters.time.to_f64().unwrap(),
                self.parameters.cosmo_params,
            ))
            .unwrap();
        }

        // If finished, wait for I/O to finish
        if !self.not_finished() {
            // If using remote storage, get url pairs and print them
            #[cfg(feature = "remote-storage")]
            while !self.active_io.is_empty() {
                let _grid = self
                    .parameters
                    .remote_storage
                    .rt
                    .block_on(self.active_io.remove(0))
                    .expect("remote io failed");
                // println!("uploaded {grid}");
            }

            // If using local storage, wait for io to finish
            #[cfg(not(feature = "remote-storage"))]
            {
                let done_threads = drain_filter(&mut self.active_io, |io| io.is_finished());

                for _io in done_threads {
                    // println!("I/O took {} millis", io.join().unwrap());
                }
            }
            self.pb.finish();
        }

        Ok(())
    }

    /// This function computes the max timestep we can take, a constraint given by the minimum
    /// of the maximum kinetic, potential timesteps such that the wavefunction phase moves by >=2pi.
    #[cfg(not(feature = "expanding"))]
    pub fn get_timestep(&self) -> (bool, T) {
        // Max kinetic dt
        // max(k^2)/2
        let kinetic_dt: T =
            self.parameters.cfl * T::from_f64(2.0).unwrap() * self.parameters.axis_length
                / self.parameters.k2_max.sqrt()
                / self.parameters.hbar_;
        debug_assert!(
            kinetic_dt.is_finite(),
            "kinetic_dt is {}; hbar_ is {}",
            kinetic_dt,
            self.parameters.hbar_
        );
        debug_assert!(
            kinetic_dt.is_sign_positive(),
            "kinetic_dt is {}; hbar_ is {}",
            kinetic_dt,
            self.parameters.hbar_
        );
        debug_assert!(
            !kinetic_dt.is_zero(),
            "kinetic_dt is {}; hbar_ is {}",
            kinetic_dt,
            self.parameters.hbar_
        );

        // Max potential dt
        let potential_max: T = max_all(&abs(&self.grid.φ)).0;
        let potential_dt: T = self.parameters.cfl
            * T::from_f64(2.0 * std::f64::consts::PI).unwrap()
            * self.parameters.hbar_
            / (T::from_f64(2.0).unwrap() * potential_max);
        debug_assert!(potential_dt.is_finite());
        debug_assert!(potential_dt.is_sign_positive());
        debug_assert!(!potential_dt.is_zero());

        // Time to next data dump
        // TODO: fix for initial_time != 0
        let time_to_next_dump = (T::from_u32(self.parameters.current_dumps + 1).unwrap()
            * self.parameters.final_sim_time
            / T::from_u32(self.parameters.num_data_dumps).unwrap())
            - self.parameters.time;

        // Take smallest of all time steps
        let dt = kinetic_dt.min(potential_dt).min(time_to_next_dump);
        // println!("kinetic = {:.4e}; potential = {:.4e}; kinetic/potential = {}; time to next {time_to_next_dump:.4e}", kinetic_dt, potential_dt, kinetic_dt/potential_dt);

        // If taking time_to_next_dump, return dump flag
        let mut dump = false;
        if dt == time_to_next_dump {
            dump = true;
            // println!("dump dt");
        }

        // Return dump flag and timestep
        (dump, dt)
    }

    /// This function computes the max timestep we can take, a constraint given by the minimum
    /// of the maximum kinetic, potential timesteps such that the wavefunction phase moves by >=2pi.
    #[cfg(feature = "expanding")]
    pub fn get_timestep(&self) -> (bool, T) {
        // Max kinetic dtau
        // max(k^2)/2
        let kinetic_dtau: T =
            self.parameters.cfl * T::from_f64(2.0).unwrap() * self.parameters.comoving_boxsize
                / self.parameters.k2_max.sqrt();
        debug_assert!(kinetic_dtau.is_finite(), "kinetic_dtau is {}", kinetic_dtau,);
        debug_assert!(
            kinetic_dtau.is_sign_positive(),
            "kinetic_dtau is {}",
            kinetic_dtau,
        );
        debug_assert!(!kinetic_dtau.is_zero(), "kinetic_dtau is {}", kinetic_dtau,);

        // Max potential dtau
        let potential_max: T = max_all(&abs(&self.grid.φ)).0;
        // log::debug!("potential_max is {potential_max}");
        // panic!("potential_max is {potential_max:.50}");
        let potential_dtau: T = self.parameters.cfl
            * T::from_f64(2.0 * std::f64::consts::PI).unwrap()
            / (T::from_f64(2.0 * self.scale_factor_solver.get_a()).unwrap() * potential_max);
        debug_assert!(potential_dtau.is_finite());
        debug_assert!(potential_dtau.is_sign_positive());
        debug_assert!(!potential_dtau.is_zero());

        // Time to next data dump
        // TODO: fix for initial_time != 0
        let time_to_next_dump = (T::from_u32(self.parameters.current_dumps + 1).unwrap()
            * self.parameters.final_sim_time
            / T::from_u32(self.parameters.num_data_dumps).unwrap())
            - self.parameters.time;
        let tau_to_next_dump = T::from(get_tau(
            (self.parameters.time + time_to_next_dump).to_f64().unwrap(),
            self.parameters.cosmo_params,
        ))
        .unwrap()
            - self.parameters.tau;

        // Take smallest of all time steps
        let dtau = kinetic_dtau.min(potential_dtau).min(tau_to_next_dump);
        // println!("kinetic = {:.4e}; potential = {:.4e}; kinetic/potential = {}; time to next {time_to_next_dump:.4e}", kinetic_dtau, potential_dtau, kinetic_dtau/potential_dtau);

        // If taking time_to_next_dump, return dump flag
        let mut dump = false;
        if dtau == tau_to_next_dump {
            dump = true;
            // println!("dump dtau {}", self.parameters.current_dumps);
        }

        // Return dump flag and timestep
        (dump, dtau)
    }

    /// This function computes the shape of the grid
    pub fn get_shape(&self) -> (u64, u64, u64, u64) {
        match self.parameters.dims {
            Dimensions::One => (self.parameters.size as u64, 1, 1, 1),
            Dimensions::Two => (
                self.parameters.size as u64,
                self.parameters.size as u64,
                1,
                1,
            ),
            Dimensions::Three => (
                self.parameters.size as u64,
                self.parameters.size as u64,
                self.parameters.size as u64,
                1,
            ),
        }
    }

    // This function computes the shape of the grid
    pub fn get_shape_array(&self) -> [u64; 4] {
        match self.parameters.dims {
            Dimensions::One => [self.parameters.size as u64, 1, 1, 1],
            Dimensions::Two => [
                self.parameters.size as u64,
                self.parameters.size as u64,
                1,
                1,
            ],
            Dimensions::Three => [
                self.parameters.size as u64,
                self.parameters.size as u64,
                self.parameters.size as u64,
                1,
            ],
        }
    }

    /// This function computes the space density
    pub fn calculate_density(&mut self) {
        #[cfg(feature = "expanding")]
        let expanding_factor: T = {
            T::from(
                self.parameters.total_mass
                    * POIS_CONST
                    * (2.0
                        / (3.0
                            * (self.parameters.cosmo_params.h * LITTLE_H_TO_BIG_H).powi(2)
                            * self.parameters.cosmo_params.omega_matter_now))
                        .powf(1.0 / 4.0),
            )
            .unwrap()
                / self
                    .parameters
                    .hbar_
                    .powf(T::from(self.parameters.dims as i64 as f64 / 2.0).unwrap())
        };

        // We reuse the memory for φ to store the density
        self.grid.φ = mul(
            &Array::new(
                #[cfg(feature = "expanding")]
                &[expanding_factor],
                #[cfg(not(feature = "expanding"))]
                &[T::from_f64(self.parameters.total_mass).unwrap()],
                Dim4::new(&[1, 1, 1, 1]),
            ),
            &real(&mul(&self.grid.ψ, &conjg(&self.grid.ψ), false)),
            true,
        )
        .cast();
    }

    /// This function calculates the potential for the stream
    pub fn calculate_potential(&mut self) {
        // Compute space density and perform inplace fft
        // note: this is using memory location of self.grid.φ, overwriting previous value.
        self.calculate_density();
        debug_assert!(check_complex_for_nans(&self.grid.φ));
        self.forward_potential_inplace()
            .expect("failed to do forward fft for density");
        debug_assert!(check_complex_for_nans(&self.grid.φ));

        // Compute potential in k-space
        self.grid.φ = div(
            &mul(
                &Array::new(
                    &[Complex::<T>::new(
                        #[cfg(feature = "expanding")]
                        // laplacian phi = |psi|^2 -1 implies phi(k) = density / (ik)^2
                        -T::one(),
                        #[cfg(not(feature = "expanding"))]
                        // laplacian phi = POIS_CONST * (|psi|^2 -1)  implies phi(k) = POIS_CONST * density / (ik)^2
                        T::from_f64(-POIS_CONST).unwrap(),
                        T::zero(),
                    )],
                    Dim4::new(&[1, 1, 1, 1]),
                ),
                &self.grid.φ, // consistent with the note above, at this point in the code this is the density.
                true,
            ),
            &self.parameters.spec_grid.cast(),
            false,
        );

        // Populate 0 mode with 0.0
        let cond = isnan(&self.grid.φ);
        let value = [false];
        let cond: Array<bool> =
            arrayfire::eq(&cond, &Array::new(&value, Dim4::new(&[1, 1, 1, 1])), true);
        replace_scalar(&mut self.grid.φ, &cond, 0.0);

        // Perform inverse fft inplace
        self.inverse_potential_inplace()
            .expect("failed to do inverse fft for potential");
        debug_assert!(check_complex_for_nans(&self.grid.φ));

        self.grid.φ = real(&self.grid.φ).cast();
    }

    /// This function writes out the wavefunction and metadata to disk
    pub fn dump(&mut self) {
        // Create directory if necessary
        #[cfg(not(feature = "remote-storage"))]
        const SIM_DATA_FOLDER: &'static str = "sim-data";

        #[cfg(not(feature = "remote-storage"))]
        std::fs::create_dir_all(format!("{SIM_DATA_FOLDER}/{}/", self.parameters.sim_name))
            .expect("failed to make directory");

        // Check to see which are active
        while self.active_io.len() >= MAX_CONCURRENT_GRID_WRITES * 2 {
            // factor of 2 is here for real + imag

            std::thread::sleep(std::time::Duration::from_millis(10));

            // Steal all done threads from active_io
            #[cfg(not(feature = "remote-storage"))]
            {
                let done_threads = drain_filter(&mut self.active_io, |io| io.is_finished());

                for _io in done_threads {
                    // println!("I/O took {} millis", io.join().unwrap());
                }
            }
            #[cfg(feature = "remote-storage")]
            {
                let _grid = self
                    .parameters
                    .remote_storage
                    .rt
                    .block_on(self.active_io.remove(0))
                    .expect("io failed");
                // println!("uploaded {grid}");
            }
        }

        #[cfg(not(feature = "remote-storage"))]
        {
            let shape = self.get_shape_array();

            self.active_io.append(
                &mut complex_array_to_disk(
                    format!(
                        "{SIM_DATA_FOLDER}/{}/psi_{:05}",
                        self.parameters.sim_name, self.parameters.current_dumps
                    ),
                    &self.grid.ψ,
                    shape,
                )
                .map_err(|_| RuntimeError::IOError)
                .unwrap(),
            );

            // output potential
            if self.parameters.output_potential {
                self.calculate_potential(); // TODO; might be redundant but its ok for now
                self.active_io.append(
                    &mut complex_array_to_disk(
                        format!(
                            "{SIM_DATA_FOLDER}/{}/potential_{:05}",
                            self.parameters.sim_name, self.parameters.current_dumps
                        ),
                        &self.grid.φ,
                        shape,
                    )
                    .unwrap(),
                );
            }
        }

        #[cfg(feature = "remote-storage")]
        {
            // Upload to remote storage
            let filename = format!(
                "{}_psi_{:05}",
                self.parameters.sim_name, self.parameters.current_dumps
            );
            let active_io = self
                .parameters
                .remote_storage
                .upload_grid(&self.grid.ψ, filename);
            self.active_io.push(active_io);

            if self.parameters.output_potential {
                self.calculate_potential(); // TODO; might be redundant but its ok for now
                let filename = format!(
                    "{}_potential_{:05}",
                    self.parameters.sim_name, self.parameters.current_dumps
                );
                let active_io = self
                    .parameters
                    .remote_storage
                    .upload_grid(&self.grid.φ, filename);
                self.active_io.push(active_io);
            }
        }

        #[cfg(feature = "expanding")]
        self.pb.set_message(format!(
            "({}) z = {}",
            &self.parameters.sim_name,
            self.get_scale_factor().recip() - 1.0
        ));
        #[cfg(not(feature = "expanding"))]
        self.pb.set_message(format!(
            "({}) t = {}",
            &self.parameters.sim_name,
            self.parameters.time.to_f64().unwrap()
        ));
        self.pb.inc(1);
    }

    /// This function checks if simulation is done
    pub fn not_finished(&self) -> bool {
        self.parameters.time < self.parameters.final_sim_time
    }

    /// This function outputs a text file
    // pub fn dump_parameters(&self, additional_parameters: Vec<String>) {
    //     // Create directory if necessary
    //     std::fs::create_dir_all(format!("sim-data/{}/", self.parameters.sim_name))
    //         .expect("failed to make directory");

    //     // Location of parameter file
    //     let param_file: String = format!("sim-data/{}/parameters.txt", self.parameters.sim_name);

    //     // Write to parameter file
    //     std::fs::write(
    //         param_file,
    //         format!("{}{}", self.parameters, additional_parameters.join("")),
    //     )
    //     .expect("Failed to write parameter file");
    // }

    /// This function checks the Fourier space wavefunction for aliasing
    /// TODO, do this check in place to save memory
    pub fn check_alias(&self) -> Option<T> {
        // Clone the Fourier space wavefunction
        let alias_check = self.grid.ψk.copy();
        debug_assert!(crate::utils::grid::check_norm::<T>(
            &alias_check,
            self.parameters.dk,
            self.parameters.dims
        ));

        // Norm squared, cast to real
        let mut alias_check: Array<T> = real(&mul(&alias_check, &conjg(&alias_check), false));

        // Replace all values under cutoff with 0
        let is_over_cutoff = arrayfire::gt(
            &self.parameters.spec_grid,
            &arrayfire::constant(
                self.parameters.k2_max * T::from_f64(self.parameters.k2_cutoff).unwrap(),
                Dim4::new(&[1, 1, 1, 1]),
            ),
            true,
        );
        replace_scalar::<T>(
            // Array to replace
            &mut alias_check,
            // Condition to check for
            &is_over_cutoff,
            // Value to replace with when false
            0.0,
        );

        // Sum all remaining values (those over cutoff) to get total mass that is near-aliasing
        let sum = arrayfire::sum_all(&alias_check);
        let p_mass = sum.0
            * self
                .parameters
                .dk
                .powf(T::from_usize(self.parameters.dims as usize).unwrap());

        // If above threshold, return Some. Otherwise, return None
        if p_mass > T::from_f64(self.parameters.alias_threshold).unwrap() {
            Some(p_mass)
        } else {
            None
        }
    }

    // fn forward_inplace(&mut self, array: &mut Array<Complex<T>>) -> Result<()> {
    //     let dims = self.parameters.dims;
    //     let size = self.parameters.size;
    //     forward_inplace(array, dims, size)
    // }

    // fn inverse_inplace(&mut self, array: &mut Array<Complex<T>>) -> Result<()> {
    //     let dims = self.parameters.dims;
    //     let size = self.parameters.size;
    //     inverse_inplace(array, dims, size)
    // }

    fn forward_potential_inplace(&mut self) -> Result<()> {
        let dims = self.parameters.dims;
        let size = self.parameters.size;
        forward_inplace(&mut self.grid.φ, dims, size)
    }

    fn inverse_potential_inplace(&mut self) -> Result<()> {
        let dims = self.parameters.dims;
        let size = self.parameters.size;
        inverse_inplace(&mut self.grid.φ, dims, size)
    }

    fn forward(&self, array: &Array<Complex<T>>) -> Result<Array<Complex<T>>> {
        let dims = self.parameters.dims;
        let size = self.parameters.size;
        forward(array, dims, size)
    }

    fn inverse(&self, array: &Array<Complex<T>>) -> Result<Array<Complex<T>>> {
        let dims = self.parameters.dims;
        let size = self.parameters.size;
        inverse(array, dims, size)
    }

    #[cfg(feature = "expanding")]
    fn get_scale_factor(&self) -> f64 {
        self.scale_factor_solver.get_a()
    }

    #[cfg(feature = "expanding")]
    #[allow(non_snake_case)]
    fn get_scale_factor_T(&self) -> T {
        T::from_f64(self.scale_factor_solver.get_a()).unwrap()
    }

    /// This function calculates the RK4 dt for a given dtau.
    #[cfg(feature = "expanding")]
    fn calculate_dt_from_dtau(&self, dtau: T) -> T {
        unsafe {
            // Clone current solver state
            let solver_state = std::cell::UnsafeCell::new(self.scale_factor_solver.clone());

            // Define derivative_function dt_dtau
            let dt_dtau = |_tau: f64, t: f64| -> f64 {
                // Get a at this time
                let a_at_t = {
                    let solver_step = t - (*solver_state.get()).get_time();
                    (*solver_state.get()).step(solver_step)
                };

                // reciprocal of (3/2 * H_0^2 * Omega_m0)^(1/2) / a^2
                ((1.5
                    * self.parameters.cosmo_params.omega_matter_now
                    * (LITTLE_H_TO_BIG_H * self.parameters.cosmo_params.h).powi(2))
                .sqrt()
                    / a_at_t.powi(2))
                .recip()
            };

            let dt = T::from(rk4(
                dt_dtau,
                self.parameters.tau.to_f64().unwrap(),
                self.parameters.time.to_f64().unwrap(),
                dtau.to_f64().unwrap(),
                None,
            ))
            .unwrap()
            .sub(self.parameters.time);

            // TODO: remove after all debugging is complete
            // let approx_init_dt = ((1.5
            //     * self.parameters.cosmo_params.omega_matter_now
            //     * (LITTLE_H_TO_BIG_H * self.parameters.cosmo_params.h).powi(2))
            // .sqrt()
            //     / (1.0 / 101.0).powi(2))
            // .recip()
            //     * dtau.to_f64().unwrap();
            // println!("dt = {dt}, dtau = {dtau}, approx dt = {approx_init_dt}");

            dt
        }
    }
}

#[test]
fn test_new_grid() {
    use arrayfire::Dim4;

    // Grid parameters
    const S: usize = 32;

    // Random wavefunction
    let values = [Complex::<f32>::new(1.0, 2.0); S];
    let dims = Dim4::new(&[S as u64, 1, 1, 1]);
    let ψ: Array<Complex<f32>> = Array::new(&values, dims);

    // Initialize grid
    let _grid: SimulationGrid<f32> = SimulationGrid::<f32>::new(ψ);
}

#[cfg(feature = "expanding")]
fn get_tau(target_time: f64, cosmo_params: CosmologyParameters) -> f64 {
    // SAFETY: this is safe because accesses are done sequentially on one thread.
    // This is only used because the function dtau_dt pased into the scale factor solver expects
    // an Fn(..) not an FnMut(..). Other primitives like a Mutex *could* be used.
    // Only the .get()s are unsafe methods but a block is used for readability.
    unsafe {
        // Initialize cosmo solver.
        // This is done using a an unsafe cell due to restrictions on Fn(f64, f64)
        let scale_factor_solver = std::cell::UnsafeCell::new(ScaleFactorSolver::new(cosmo_params));

        let dtau_dt = |t: f64, _tau: f64| -> f64 {
            // Get a for this time
            let a_at_t = {
                let solver_step = t - (*scale_factor_solver.get()).get_time();
                (*scale_factor_solver.get()).step(solver_step)
            };

            // (3/2 * H_0^2 * Omega_m0)^(1/2) / a^2
            (1.5 * cosmo_params.omega_matter_now * (LITTLE_H_TO_BIG_H * cosmo_params.h).powi(2))
                .sqrt()
                / a_at_t.powi(2)
        };

        // Initialize tau(t=0) = 0; z(t=0) = z0
        let mut tau = 0.0;
        let mut time = 0.0;

        let mut dt;
        while time < target_time {
            dt = target_time / 1000.0;
            if let Some(max_dloga) = cosmo_params.max_dloga {
                dt = (target_time / 1000.0).min(
                    (*scale_factor_solver.get()).get_a() / (*scale_factor_solver.get()).get_dadt()
                        * max_dloga,
                );
            }
            dt = dt.min(target_time - time);

            // Step tau and time forward
            tau = rk4(&dtau_dt, time, tau, dt, None);
            time += dt;
        }

        tau
    }
}

/// This is not yet on the stable channel
#[cfg(not(feature = "remote-storage"))]
fn drain_filter<T, P>(items: &mut Vec<T>, predicate: P) -> Vec<T>
where
    P: Fn(&T) -> bool,
{
    let mut drained_values = vec![];
    let mut i = 0;
    while i < items.len() {
        if predicate(&items[i]) {
            drained_values.push(items.swap_remove(i))
        } else {
            i += 1;
        }
    }
    drained_values
}
