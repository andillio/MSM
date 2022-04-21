use arrayfire::{Dim4, Array, HasAfEnum};
use num::{Float, Complex};
use ndarray_npy::WritableElement;
use super::error::RuntimeError;
use anyhow::{Result, Context};



/// This function writes an arrayfire array to disk in .npy format. It first hosts the
pub fn complex_array_to_disk<T>(
    path: &str,
    name: &str,
    array: &Array<Complex<T>>,
    shape: [u64; 4],
) -> Result<()>
where
    T: Float + HasAfEnum + WritableElement,
    Complex<T>: HasAfEnum,
{
    // Host array
    let mut host = vec![Complex::<T>::new(T::zero(), T::zero()); array.elements()];
    array.host(&mut host);
    let real: Vec<T> = host
        .iter()
        .map(|x| x.re)
        .collect();
    let imag: Vec<T> = host
        .iter()
        .map(|x| x.im)
        .collect();
    
 
     // Build nd_array for npy serialization
     let real: ndarray::Array1<T> = ndarray::ArrayBase::from_vec(real);
     let imag: ndarray::Array1<T> = ndarray::ArrayBase::from_vec(imag);
     let real = real.into_shape(array_to_tuple(shape)).unwrap();
     let imag = imag.into_shape(array_to_tuple(shape)).unwrap();
     //println!("host shape is now {:?}", real.shape());
 
     // Write to npz
     use ndarray_npy::NpzWriter;
     use std::fs::File;
     let mut npz = NpzWriter::new(File::create(path).unwrap());
     npz.add_array("real", &real).context(RuntimeError::IOError)?;
     npz.add_array("imag", &imag).context(RuntimeError::IOError)?;
     npz.finish().context(RuntimeError::IOError)?;
     Ok(())
}

pub fn array_to_disk<T>(
    path: &str,
    name: &str,
    array: &Array<T>,
    shape: [u64; 4],
) -> Result<()>
where
    T: Float + HasAfEnum + WritableElement,
{
     // Host array
     let mut host = vec![T::one(); array.elements()];
     array.host(&mut host);
 
     // Build nd_array for npy serialization
     let host: ndarray::Array1<T> = ndarray::ArrayBase::from_vec(host);
     let host = host.into_shape(array_to_tuple(shape)).unwrap();
     println!("host shape is now {:?}", host.shape());
 
     // Write to npz
     use ndarray_npy::NpzWriter;
     use std::fs::File;
     let mut npz = NpzWriter::new(File::create(path).unwrap());
     npz.add_array(name, &host).context(RuntimeError::IOError)?;
     npz.finish().context(RuntimeError::IOError)?;
     Ok(())
}

/// This is a helper function to turn a length 4 array (required by Dim4) into a tuple,
/// which is required by ndarray::Array's .reshape() method
pub fn array_to_tuple(
    dim4: [u64; 4],
) -> (usize, usize, usize, usize) {
    (dim4[0] as usize, dim4[1] as usize, dim4[2] as usize, dim4[3] as usize)
}

/// This is a helper function to turn a length 4 array (required by Dim4) into a Dim4,
pub fn array_to_dim4(
    dim4: [u64; 4],
) -> Dim4 {
    Dim4::new(&dim4)
}