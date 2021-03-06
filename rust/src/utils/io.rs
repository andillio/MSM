use arrayfire::{Dim4, Array, HasAfEnum};
use num::{Float, Complex};
use ndarray_npy::{WritableElement, write_npy};
use anyhow::{Result, Context};
use serde::de::DeserializeOwned;

/// This function writes an arrayfire array to disk in .npy format. It first hosts the
pub async fn complex_array_to_disk<T>(
    path: &str,
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
    //  let mut npz = NpzWriter::new(File::create(path).unwrap());
    //  npz.add_array("real", &real).context(RuntimeError::IOError)?;
    //  npz.add_array("imag", &imag).context(RuntimeError::IOError)?;
    //  npz.finish().context(RuntimeError::IOError)?;

    let real_path = format!("{}_real", path);
    let imag_path = format!("{}_imag", path);
    let real = array_to_disk(real_path, &real);
    let imag = array_to_disk(imag_path, &imag);
    futures::join!(real, imag).expect("write to disk in parallel failed");

    Ok(())
}

async fn array_to_disk<T>(
    path: String,
    array: &ndarray::Array4<T>,
) -> Result<()>
where
    T: Float + HasAfEnum + WritableElement,
{
     // Write to npy
     write_npy(path, array).expect("write to disk failed");
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

/// This function reads toml files
pub fn read_toml<T: DeserializeOwned>(
    path: String
) -> T {

    // Read toml config file
    let toml_contents: &str = &std::fs::read_to_string(&path).expect(format!("Unable to load toml and parse as string: {}", &path).as_str());

    // Return parsed toml from str
    toml::from_str(toml_contents).expect("Could not parse toml file contents")
}