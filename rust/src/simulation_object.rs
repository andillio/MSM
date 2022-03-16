use arrayfire::{Array, ComplexFloating, HasAfEnum, FloatingPoint, c32, Dim4};
use crate::utils::fft::forward;
use conv::*;
use std::fmt::Display;
use num::{Complex, Float, FromPrimitive};

/// This struct holds the grids which store the wavefunction and its Fourier transform
pub struct SimulationGrid<T>
where
    T: Float + FloatingPoint,
    Complex<T>: HasAfEnum + FloatingPoint,
    <Complex<T> as arrayfire::HasAfEnum>::ComplexOutType: HasAfEnum
{

    // Spatial Fields
    /// The array which stores the wavefunction
    pub ψ: Array<Complex<T>>,

    /// Fourier space
    pub ψk: Array<<Complex<T> as arrayfire::HasAfEnum>::ComplexOutType>,
}



/// This `Parameters` struct stores simulations parameters
pub struct SimulationParameters<U: Float + FloatingPoint> {

    // Grid Parameters
    /// Number of pixels per axis
    pub n_grid: u32,
    /// Physical length of each axis
    pub axis_length: U,
    /// Spatial cell size
    pub dx: U,
    /// k-space cell size
    pub dk: U,

    // Temporal Parameters
    /// Total simulation time
    pub total_sim_time: U,
    /// Number of data dumps
    pub num_data_dumps: u32,

    // Physical Parameters
    /// Total Mass
    pub total_mass: U,
    /// Particle mass
    pub particle_mass: U,

    // Metadata
    /// Simulation name
    pub sim_name: &'static str,
}

/// In the original python implementation, this was a `sim` or `SimObject` object.
/// This stores a `SimulationGrid` which has the wavefunction and its fourier transform.
/// It also holds the `SimulationParameters` which holds the simulation parameters.
pub struct SimulationObject<T>
where
    T: Float + FloatingPoint,
    Complex<T>: HasAfEnum + ComplexFloating + FloatingPoint,
    <Complex<T> as arrayfire::HasAfEnum>::ComplexOutType: HasAfEnum,
{

    /// This has the wavefunction and its Fourier transform
    pub grid: SimulationGrid<T>,

    /// This has the simulation parameters
    pub parameters: SimulationParameters<T>,

}

impl<T> SimulationGrid<T>
where
    T: Float + FloatingPoint,
    Complex<T>: HasAfEnum + ComplexFloating + FloatingPoint,
    <Complex<T> as arrayfire::HasAfEnum>::ComplexOutType: HasAfEnum
 {

    pub fn new<const K: usize, const S: usize>(
        ψ: Array<Complex<T>>, 
    ) -> Self
    where
        T: Float,
        Complex<T>: HasAfEnum + ComplexFloating + FloatingPoint,
        <Complex<T> as arrayfire::HasAfEnum>::ComplexOutType: HasAfEnum
    {
        let ψk = forward::<T, K, S>(&ψ).expect("failed forward fft");
        SimulationGrid {
            ψ,
            ψk,
        }
    }

}

impl<U> SimulationParameters<U>
where
    U: FromPrimitive + Float + FloatingPoint + Display
{

    pub fn new(
        n_grid: u32,
        axis_length: U,
        total_sim_time: U,
        num_data_dumps: u32,
        total_mass: U,
        particle_mass: U,
        sim_name: &'static str,

    ) -> Self
    {

        // Overconstrained parameters 
        let dx = axis_length / U::from_u32(n_grid).unwrap();
        let dk = U::from_f64(2.0).unwrap() * U::from_f64(std::f64::consts::PI).unwrap() / dx;

        SimulationParameters {
            n_grid,
            axis_length,
            dx,
            dk,
            total_sim_time,
            num_data_dumps,
            total_mass,
            particle_mass,
            sim_name, 
        }
    }

    pub fn update(){

    }
}
impl<U> Display for SimulationParameters<U>
where
    U: ValueFrom<u32> + ValueFrom<f64> + Float + FloatingPoint + Display
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n","-".repeat(40))?;
        write!(f, "n_grid         = {}\n", self.n_grid)?;
        write!(f, "axis_length    = {}\n", self.axis_length)?;
        write!(f, "dx             = {}\n", self.dx)?;
        write!(f, "dk             = {}\n", self.dk)?;
        write!(f, "total_sim_time = {}\n", self.total_sim_time)?;
        write!(f, "num_data_dumps = {}\n", self.num_data_dumps)?;
        write!(f, "total_mass     = {}\n", self.total_mass)?;
        write!(f, "particle_mass  = {}\n", self.particle_mass)?;
        write!(f, "sim_name       = {}\n", self.sim_name,)?;
        write!(f, "{}\n","-".repeat(40))?;
        Ok(())
    }
}



impl<T> SimulationObject<T>
where
    T: Float + FloatingPoint + Display + FromPrimitive,
    Complex<T>: HasAfEnum + FloatingPoint + ComplexFloating,
    <Complex<T> as arrayfire::HasAfEnum>::ComplexOutType: HasAfEnum
{

    pub fn new<const K: usize, const S: usize>(
        ψ: Array<Complex<T>>,
        n_grid: u32,
        axis_length: T,
        total_sim_time: T,
        num_data_dumps: u32,
        total_mass: T,
        particle_mass: T,
        sim_name: &'static str,
    ) -> Self {
        
        // Construct components
        let grid = SimulationGrid::new::<K, S>(ψ);
        let parameters = SimulationParameters::<T>::new(
            n_grid,
            axis_length,
            total_sim_time,
            num_data_dumps,
            total_mass,
            particle_mass,
            sim_name,
        );

        SimulationObject {
            grid,
            parameters,
        }
    }
}


#[test]
fn test_new_grid() {

    use arrayfire::af_print;

    // Grid parameters
    const K: usize = 1;
    const S: usize = 32;

    // Random wavefunction
    let values = [Complex::<f32>::new(1.0, 2.0); S];
    let dims = Dim4::new(&[S as u64, 1, 1, 1]);
    let ψ: Array<Complex<f32>> = Array::new(&values, dims);

    // Initialize grid
    let grid: SimulationGrid<f32> = SimulationGrid::<f32>::new::<K, S>(ψ);
    af_print!("ψ", grid.ψ);
    af_print!("ψk", grid.ψk);
}


#[test]
fn test_new_sim_parameters() {

    type T = f64;

    let n_grid: u32 = 16;
    let axis_length: T = 1.0; 
    let total_sim_time: T = 1.0;
    let num_data_dumps: u32 = 100;
    let total_mass: T = 1.0;
    let particle_mass: T = 1e-6;
    let sim_name: &'static str = "my-sim";

    let params = SimulationParameters::new(
        n_grid,
        axis_length,
        total_sim_time,
        num_data_dumps,
        total_mass,
        particle_mass,
        sim_name,
    );
    println!("{}", params);
}


