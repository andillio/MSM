//use msm::run_simulation;
use arrayfire::{set_device,device_info};
use msm::simulation_object::*;
use msm::ics;
use std::time::Instant;
use std::convert::TryInto;

fn main() {
     
     set_device(0);
     device_info();

     // Set size
     const K: usize = 3;
     const S: usize = 512;
     type T = f64;


     // From slack (Andrew)
     // Mtot = 1e14
     // L = 100 kpc
     // hbar_ = 0.02 
     // v0 = 0.03 kpc / Myr
     // T = 625 Myr?
     // m = 1e-22
     let hbar_ = Some(0.02);

     const NSTREAMS: i64 = 64;
     //let streams = (0..=NSTREAMS).map(|x| (x - 1) % (NSTREAMS + 1));
     //let streams = (0..1_i64).chain(45..=NSTREAMS).map(|x| (x - 1).rem_euclid(NSTREAMS+1));
     //let streams = (0..16_i64).map(|x| (x - 1).rem_euclid(NSTREAMS+1));
     //let streams = (16..32_i64).map(|x| (x - 1).rem_euclid(NSTREAMS+1));
     //let streams = (32..48_i64).map(|x| (x - 1).rem_euclid(NSTREAMS+1));
     let streams = (48..=NSTREAMS).map(|x| (x - 1).rem_euclid(NSTREAMS+1));
     for stream in streams {
     
          println!("Working on stream {}", stream);
          let now = Instant::now();

          // Simulation Parameters
          let axis_length: T = 100.0; 
          let time: T = 0.0;
          let cfl: T = 0.40;
          let num_data_dumps: u32 = 200;
          let total_mass: f64 = 1e14 * 1e1;
          let particle_mass: f64 = msm::constants::HBAR / hbar_.unwrap();
          let sim_name: String = if stream != NSTREAMS { format!("bose-star-{}-stream{:05}", S, stream) } else { format!("bose-star-{}", S) };
          let total_sim_time: T = 625.0;
          //let total_sim_time: T = axis_length.powf(2.0) / (msm::constants::HBAR/particle_mass) as f32 / 1000.0;//200.0; //500000.0;
          let k2_cutoff = 0.95;
          let alias_threshold = 0.02;
          


          let params = SimulationParameters::<T, K, S>::new(
               axis_length,
               time,
               total_sim_time,
               cfl,
               num_data_dumps,
               total_mass,
               particle_mass,
               sim_name,
               k2_cutoff,
               alias_threshold,
               hbar_
          );

          // Overconstrained parameters
          let kinetic_max = params.k2_max as f64/ 2.0;
          let kinetic_dt = 2.0 * std::f64::consts::PI / (kinetic_max * msm::constants::HBAR/particle_mass);
          let d?? = kinetic_dt*(msm::constants::HBAR/particle_mass)/axis_length.powf(2.0) as f64;
          let ??: f64 = total_mass*msm::constants::POIS_CONST*axis_length.powf(4.0 - K as T) as f64/(msm::constants::HBAR/particle_mass).powf(2.0);
          println!("d?? = {:e}, ?? = {:e}, ??T = {:e}", d??, ??, total_sim_time);

          // Gaussian Parameters
          let k_width: T = 0.03 / hbar_.unwrap() as T;
          let mean: [T; 3] = [0.0; 3];
          let std: [T; 3] = [k_width; 3];

          // Construct SimulationObject
          let phase_seed = Some(0);
          let sample_seed = Some(stream.try_into().unwrap());
          let mut simulation_object = if stream != NSTREAMS {
                    ics::cold_gauss_kspace_sample::<T, K, S>(
                         mean, 
                         std, 
                         params, 
                         ics::SamplingScheme::Husimi,
                         phase_seed, 
                         sample_seed)
          } else {
               ics::cold_gauss_kspace::<T, K, S>(
                    mean, 
                    std, 
                    params, 
                    phase_seed,
               )
          };

          // Define additional parameters
          let additional_parameters: Vec<String> = if stream != NSTREAMS { 
               vec![
                    format!("phase_seed          = {}\n", phase_seed.unwrap()),
                    format!("sample_seed         = {}\n", sample_seed.unwrap()),
               ]
          } else {
               vec![
                    format!("phase_seed          = {}\n", phase_seed.unwrap()),
               ]
          };

          // Dump initial conditions and parameter file
          simulation_object.dump();
          simulation_object.dump_parameters(additional_parameters);
          
          // debug_assert!(msm::utils::grid::check_complex_for_nans(&simulation_object.grid.??));

          let verbose = false;
          while simulation_object.not_finished() {
               simulation_object.update(verbose).expect("failed to update");
          }
          assert!(!!!simulation_object.not_finished());

          println!("Finished stream {} in {} seconds", stream, now.elapsed().as_secs());
     }
}
